import { projectKey } from "@bibcode/client-runtime/state/entities";
import type {
  ContextMenuItem,
  GitManagerBlockedReason,
  GitManagerCommitEntry,
  GitManagerCommitPage,
  ScopedProjectRef,
} from "@bibcode/contracts";
import { GitManagerOperationError } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import { AsyncResult } from "effect/reactivity";
import { RefreshCwIcon } from "lucide-react";
import {
  memo,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
} from "react";

import { DEFAULT_GIT_MANAGER_VIEW_STATE, useGitManagerStore } from "../../../gitManagerStore";
import { readLocalApi } from "../../../localApi";
import { gitManagerEnvironment } from "../../../state/gitManager";
import { useEnvironmentQuery } from "../../../state/query";
import {
  buildCommitMenuItems,
  type GitManagerCommitMenuItemId,
} from "../rewrite/GitManagerCommitContextMenu.logic";
import type { GitManagerCommitDropResolution } from "../rewrite/gitManagerCommitDrag";
import { createCommitLookup, spliceCommitGeneration, mergeCommitDecorations } from "./commitPaging";
import { GitManagerCommitDetail } from "./GitManagerCommitDetail";
import { GitManagerCommitList } from "./GitManagerCommitList";
import { Button } from "../../ui/button";
import { RetryButton } from "../../ui/retry-button";

const HISTORY_PAGE_SIZE = 100;
const COMMIT_LOOKUP_MAX_ENTRIES = 1_000;
const COMMIT_LOOKUP_MAX_MEMORY_BYTES = 32 * 1024 * 1024;
const EMPTY_COMMITS: ReadonlyArray<GitManagerCommitEntry> = Object.freeze([]);
const EMPTY_TIPS: ReadonlyArray<string> = Object.freeze([]);
const isGitManagerOperationError = Schema.is(GitManagerOperationError);

export type GitManagerHistoryAction =
  | { readonly _tag: "reset"; readonly sha: string }
  | { readonly _tag: "revert"; readonly sha: string }
  | { readonly _tag: "cherry-pick"; readonly shas: ReadonlyArray<string> }
  | {
      readonly _tag: "squash";
      readonly shas: ReadonlyArray<string>;
      readonly message: string;
    }
  | {
      readonly _tag: "reorder";
      readonly shas: ReadonlyArray<string>;
      readonly insertBeforeSha: string | null;
    }
  | { readonly _tag: "prepare-reorder"; readonly shas: ReadonlyArray<string> }
  | { readonly _tag: "create-branch"; readonly sha: string }
  | { readonly _tag: "create-tag"; readonly sha: string };

interface GitManagerHistoryViewProps {
  readonly scope: {
    readonly environmentId: ScopedProjectRef["environmentId"];
    readonly cwd: string;
  };
  readonly projectRef: ScopedProjectRef;
  readonly blockedReasons: ReadonlyArray<GitManagerBlockedReason>;
  readonly branchSyncDisabledReason: string | null;
  readonly rewriteDisabledReason: string | null;
  readonly tagDisabledReason: string | null;
  /**
   * The repository generation the panel's refs snapshot last observed. History
   * pages share that counter, so a generation past the loaded pages means a
   * commit landed and, once no first-page read is in flight, the first page is
   * refreshed and spliced. This covers in-app operations while the watcher is
   * degraded and clients without a signal. The panel refreshes refs on every
   * signal change, so this value also reports a change carried by a signal
   * change that History deferred instead of reading.
   */
  readonly repositoryGeneration: number | null;
  /** The live signal generation, or null without a live signal. Each change refreshes the first page. */
  readonly signalGeneration: number | null;
  /**
   * True while a live signal is expected but its first generation has not
   * arrived. The first read waits for it, so one read serves both triggers.
   */
  readonly signalPending: boolean;
  readonly onAction: (action: GitManagerHistoryAction) => void;
}

/** What the current first-page read accounts for; advanced during render. */
interface FirstPageRefresh {
  /** Monotonic first-page read number, null until the first read is decided. */
  readonly sequence: number | null;
  /** The signal generation the current read accounts for. */
  readonly signalGeneration: number | null;
  /** The newest repository generation that requested a read. */
  readonly repositoryGeneration: number | null;
  /**
   * The next signal change is expected to repeat what the current read covers:
   * the watcher's echo of a repository-triggered read, or the server's first
   * signature after generation 0. It defers to the repository generation
   * instead of reading.
   */
  readonly deferNextSignal: boolean;
}

/**
 * Local query-cache identity for History reads: unique per mounted view and
 * never reused, so a refresh can never be answered from an earlier read.
 */
function historyRefreshCacheKey(viewId: string, sequence: number): string {
  return `${viewId}${sequence}`;
}

const SENTENCE_END = /[.!?…]$/;

/** Ends a failure cause as a sentence, so the banner's next sentence reads correctly. */
function asSentence(text: string): string {
  const trimmed = text.trimEnd();
  return SENTENCE_END.test(trimmed) ? trimmed : `${trimmed}.`;
}

interface HistoryPagesState {
  readonly generation: number | null;
  readonly pinnedTips: ReadonlyArray<string>;
  readonly commits: ReadonlyArray<GitManagerCommitEntry>;
  readonly nextOffset: number | null;
  readonly exhausted: boolean;
  readonly degradedToAllPaging: boolean;
  readonly processedFirstPage: GitManagerCommitPage | null;
  readonly retainedRefresh: {
    /** The first-page response that captured this batch; the batch runs only while it is current. */
    readonly firstPage: GitManagerCommitPage;
    readonly pinnedTips: ReadonlyArray<string>;
    readonly offsets: ReadonlyArray<number>;
    readonly refreshCacheKey: string;
  } | null;
}

function emptyHistoryPages(): HistoryPagesState {
  return {
    generation: null,
    pinnedTips: EMPTY_TIPS,
    commits: EMPTY_COMMITS,
    nextOffset: null,
    exhausted: false,
    degradedToAllPaging: false,
    processedFirstPage: null,
    retainedRefresh: null,
  };
}

function pageSignature(page: GitManagerCommitPage | null, requestOffset = 0): string | null {
  if (page === null) return null;
  return [
    requestOffset,
    page.generation,
    page.nextOffset ?? "end",
    page.exhausted ? 1 : 0,
    page.degradedToAllPaging ? 1 : 0,
    page.pinnedTips.join(","),
    page.commits.length,
    page.commits[0]?.sha ?? "empty",
    page.commits.at(-1)?.sha ?? "empty",
    JSON.stringify(page.commits.map((commit) => commit.decorations)),
  ].join(":");
}

function historyFromFirstPage(page: GitManagerCommitPage): HistoryPagesState {
  return {
    generation: page.generation,
    pinnedTips: page.pinnedTips,
    commits: page.commits,
    nextOffset: page.nextOffset,
    exhausted: page.exhausted,
    degradedToAllPaging: page.degradedToAllPaging,
    processedFirstPage: page,
    retainedRefresh: null,
  };
}

function appendUniqueCommits(
  loaded: ReadonlyArray<GitManagerCommitEntry>,
  incoming: ReadonlyArray<GitManagerCommitEntry>,
): ReadonlyArray<GitManagerCommitEntry> {
  const loadedShas = new Set(loaded.map((commit) => commit.sha));
  const additions = incoming.filter((commit) => {
    if (loadedShas.has(commit.sha)) return false;
    loadedShas.add(commit.sha);
    return true;
  });
  return additions.length === 0 ? loaded : [...loaded, ...additions];
}

function isTipsUnresolvableFailure(result: AsyncResult.AsyncResult<unknown, unknown>): boolean {
  if (result._tag !== "Failure") return false;
  const failure = Cause.squash(result.cause);
  return isGitManagerOperationError(failure) && failure.code === "history-tips-unresolvable";
}

export const GitManagerHistoryView = memo(function GitManagerHistoryView({
  scope,
  projectRef,
  blockedReasons,
  branchSyncDisabledReason,
  rewriteDisabledReason,
  tagDisabledReason,
  repositoryGeneration,
  signalGeneration,
  signalPending,
  onAction,
}: GitManagerHistoryViewProps) {
  const { environmentId, cwd } = scope;
  const projectEnvironmentId = projectRef.environmentId;
  const projectId = projectRef.projectId;
  const storeKey = projectKey(projectRef);
  const viewId = useId();
  const [pages, setPages] = useState<HistoryPagesState>(emptyHistoryPages);
  // The page the user asked for; persisted cursors load without it.
  const [loadingOffset, setLoadingOffset] = useState<number | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [refreshEpoch, setRefreshEpoch] = useState(0);
  // The last next-page request whose pinned tips no longer resolved.
  const [tipsResetRequest, setTipsResetRequest] = useState<string | null>(null);
  const [refresh, setRefresh] = useState<FirstPageRefresh>(() => ({
    sequence: signalPending ? null : 0,
    signalGeneration,
    repositoryGeneration: null,
    deferNextSignal: signalGeneration === 0,
  }));
  // Every `setPages` writes this first, so callbacks and later effects in the
  // same flush read the latest pages.
  const pagesRef = useRef(pages);
  const processedNextPageSignatureRef = useRef<string | null>(null);
  // An explicit refresh is pending while `refreshEpoch` is ahead of this.
  const consumedRefreshEpochRef = useRef(0);
  const handledTipsResetRequestRef = useRef<string | null>(null);
  const retainedSequenceRef = useRef(0);
  const [commitLookup] = useState(() =>
    createCommitLookup<GitManagerCommitEntry>(
      COMMIT_LOOKUP_MAX_ENTRIES,
      COMMIT_LOOKUP_MAX_MEMORY_BYTES,
    ),
  );

  const selectSelectedCommitSha = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.selectedCommitSha ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.selectedCommitSha,
    [storeKey],
  );
  const selectSelectedFilePath = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.selectedFilePath ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.selectedFilePath,
    [storeKey],
  );
  const selectMultiCommitSelection = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.multiCommitSelection ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.multiCommitSelection,
    [storeKey],
  );
  const selectLoadedPageCursors = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.loadedPageCursors ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.loadedPageCursors,
    [storeKey],
  );
  const selectScrollAnchor = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.scrollAnchor ?? DEFAULT_GIT_MANAGER_VIEW_STATE.scrollAnchor,
    [storeKey],
  );
  const selectedCommitSha = useGitManagerStore(selectSelectedCommitSha);
  const selectedFilePath = useGitManagerStore(selectSelectedFilePath);
  const multiCommitSelection = useGitManagerStore(selectMultiCommitSelection);
  const loadedPageCursors = useGitManagerStore(selectLoadedPageCursors);
  const scrollAnchor = useGitManagerStore(selectScrollAnchor);
  const setSelectedCommit = useGitManagerStore((state) => state.setSelectedCommit);
  const setSelectedFile = useGitManagerStore((state) => state.setSelectedFile);
  const setMultiCommitSelection = useGitManagerStore((state) => state.setMultiCommitSelection);
  const setLoadedPageCount = useGitManagerStore((state) => state.setLoadedPageCount);
  const setLoadedPageCursors = useGitManagerStore((state) => state.setLoadedPageCursors);
  const setScrollAnchor = useGitManagerStore((state) => state.setScrollAnchor);
  const loadedPageCursorsRef = useRef(loadedPageCursors);
  useLayoutEffect(() => {
    loadedPageCursorsRef.current = loadedPageCursors;
  }, [loadedPageCursors]);

  // Advance the first-page read from the triggers during render (React's
  // "store information from previous renders" pattern), so a new read starts in
  // the same commit that observes the trigger. The first read waits until the
  // signal's availability is known; after that every signal change starts a
  // read unless it is deferred (see `deferNextSignal`). Deferring is safe: the
  // panel re-reads refs on every signal change, so a real change still arrives
  // as a repository generation past the loaded pages.
  let firstPageRefresh = refresh;
  if (firstPageRefresh.sequence === null) {
    if (!signalPending) {
      firstPageRefresh = {
        ...firstPageRefresh,
        sequence: 0,
        signalGeneration,
        deferNextSignal: signalGeneration === 0,
      };
    }
  } else if (signalGeneration !== null && signalGeneration !== firstPageRefresh.signalGeneration) {
    firstPageRefresh = firstPageRefresh.deferNextSignal
      ? { ...firstPageRefresh, signalGeneration, deferNextSignal: false }
      : { ...firstPageRefresh, sequence: firstPageRefresh.sequence + 1, signalGeneration };
  }
  if (firstPageRefresh !== refresh) setRefresh(firstPageRefresh);
  const firstPageSequence = firstPageRefresh.sequence;
  const firstPageAtom = useMemo(
    () =>
      firstPageSequence === null
        ? null
        : gitManagerEnvironment.getHistoryFirstPage({
            environmentId,
            input: {
              cwd,
              limit: HISTORY_PAGE_SIZE,
              refreshCacheKey: historyRefreshCacheKey(viewId, firstPageSequence),
            },
          }),
    [cwd, environmentId, firstPageSequence, viewId],
  );
  const firstPageQuery = useEnvironmentQuery(firstPageAtom);
  const refreshFirstPage = firstPageQuery.refresh;
  const revalidateFirstPage = firstPageQuery.revalidate;
  const firstPage = firstPageQuery.data;
  // A repository generation past everything loaded or returned (the loaded
  // pages lag the latest response by one effect) refreshes once no first-page
  // read is in flight. Each repository generation requests at most one read.
  const loadedGeneration = Math.max(pages.generation ?? -1, firstPage?.generation ?? -1);
  if (
    firstPageSequence !== null &&
    !firstPageQuery.emission.waiting &&
    loadedGeneration >= 0 &&
    repositoryGeneration !== null &&
    repositoryGeneration > loadedGeneration &&
    repositoryGeneration !== firstPageRefresh.repositoryGeneration
  ) {
    setRefresh({
      ...firstPageRefresh,
      sequence: firstPageSequence + 1,
      repositoryGeneration,
      deferNextSignal: signalGeneration !== null,
    });
  }

  // Pages loaded before a remount reload one after another. A failed page stays
  // the current request, so its cached failure is shown instead of asked again.
  const persistedNextOffset =
    pages.nextOffset !== null && !pages.exhausted && loadedPageCursors.includes(pages.nextOffset)
      ? pages.nextOffset
      : null;
  const pageLoadOffset = loadingOffset ?? persistedNextOffset;
  const nextPageAtom = useMemo(() => {
    if (pageLoadOffset === null) return null;
    const pinnedTips = pages.pinnedTips;
    return gitManagerEnvironment.getCommits({
      environmentId,
      input: {
        cwd,
        offset: pageLoadOffset,
        limit: HISTORY_PAGE_SIZE,
        ...(pinnedTips.length > 0 ? { pinnedTips } : {}),
      },
    });
  }, [cwd, environmentId, pageLoadOffset, pages.pinnedTips]);
  const nextPageQuery = useEnvironmentQuery(nextPageAtom);
  const nextPage = nextPageQuery.data;
  const retryNextPage = nextPageQuery.refresh;
  const nextPageSignature = useMemo(
    () => pageSignature(nextPage, pageLoadOffset ?? 0),
    [nextPage, pageLoadOffset],
  );
  const nextPageTipsUnresolvable = isTipsUnresolvableFailure(nextPageQuery.emission);
  const nextPageFailed =
    pageLoadOffset !== null && nextPageQuery.error !== null && !nextPageQuery.emission.waiting;
  const isLoadingMore = pageLoadOffset !== null && !nextPageFailed;
  const loadMoreError = nextPageFailed && !nextPageTipsUnresolvable ? nextPageQuery.error : null;
  // Pinned tips that no longer resolve restart history from the first page,
  // once per failed request (adjusted while rendering).
  const nextPageRequest = `${pageLoadOffset}:${pages.pinnedTips.join(",")}`;
  if (nextPageFailed && nextPageTipsUnresolvable && tipsResetRequest !== nextPageRequest) {
    setTipsResetRequest(nextPageRequest);
    setRefreshEpoch(refreshEpoch + 1);
    setLoadingOffset(null);
  }

  const retainedRefresh = pages.retainedRefresh;
  const retainedAtom = useMemo(
    () =>
      retainedRefresh === null || retainedRefresh.firstPage !== firstPage
        ? null
        : gitManagerEnvironment.getRetainedCommitPages({
            environmentId,
            input: {
              cwd,
              pinnedTips: retainedRefresh.pinnedTips,
              offsets: retainedRefresh.offsets,
              limit: HISTORY_PAGE_SIZE,
              refreshCacheKey: retainedRefresh.refreshCacheKey,
            },
          }),
    [cwd, environmentId, firstPage, retainedRefresh],
  );
  const retainedQuery = useEnvironmentQuery(retainedAtom);
  const retainedPages = retainedQuery.data;
  useEffect(() => {
    if (retainedPages === null || retainedQuery.emission.waiting) return;
    const current = pagesRef.current;
    const commits = mergeCommitDecorations(
      current.commits,
      retainedPages.flatMap((page) =>
        current.generation === null || page.generation >= current.generation ? page.commits : [],
      ),
    );
    if (commits === current.commits) return;
    for (const commit of commits) commitLookup.set(commit);
    const next = { ...current, commits };
    pagesRef.current = next;
    setPages(next);
  }, [commitLookup, retainedPages, retainedQuery.emission.waiting]);

  // Runs before the first-page effect below, which consumes the refresh.
  useEffect(() => {
    if (tipsResetRequest === null || handledTipsResetRequestRef.current === tipsResetRequest) {
      return;
    }
    handledTipsResetRequestRef.current = tipsResetRequest;
    const currentProjectRef = {
      environmentId: projectEnvironmentId,
      projectId,
    } as ScopedProjectRef;
    setLoadedPageCursors(currentProjectRef, []);
    setLoadedPageCount(currentProjectRef, 0);
    revalidateFirstPage();
  }, [
    projectEnvironmentId,
    projectId,
    revalidateFirstPage,
    setLoadedPageCount,
    setLoadedPageCursors,
    tipsResetRequest,
  ]);

  useEffect(() => {
    const page = firstPage;
    if (page === null) return;
    const current = pagesRef.current;
    const explicitRefresh = refreshEpoch !== consumedRefreshEpochRef.current;
    if (current.processedFirstPage === page && !explicitRefresh) {
      return;
    }
    if (current.generation === null || page.generation >= current.generation) {
      for (const commit of page.commits) commitLookup.set(commit);
    }

    let next: HistoryPagesState;
    let resetCursors = false;
    let preserveStoredCursors = false;
    if (explicitRefresh || current.generation === null || current.commits.length === 0) {
      next = historyFromFirstPage(page);
      resetCursors = true;
      preserveStoredCursors = current.generation === null && !explicitRefresh;
    } else if (page.generation < current.generation) {
      // A first page that resolved behind the loaded generation is a stale
      // in-flight response (an older read completing after a newer one).
      // Record it as seen without letting it regress the tip or the counter;
      // the newer page already holds the post-mutation history.
      next = { ...current, processedFirstPage: page };
    } else if (page.generation !== current.generation) {
      const spliced = spliceCommitGeneration({
        loaded: current.commits,
        incoming: page.commits,
        pinnedTips: current.pinnedTips,
      });
      if (current.degradedToAllPaging || page.degradedToAllPaging) {
        next = historyFromFirstPage(page);
        resetCursors = true;
      } else {
        next = {
          ...current,
          generation: page.generation,
          commits: spliced.commits,
          processedFirstPage: page,
        };
      }
    } else {
      next = {
        ...current,
        commits: mergeCommitDecorations(current.commits, page.commits),
        processedFirstPage: page,
      };
    }

    if (!resetCursors && current.generation !== null && page.generation >= current.generation) {
      const sameTips =
        page.pinnedTips.length === current.pinnedTips.length &&
        page.pinnedTips.every((tip, index) => tip === current.pinnedTips[index]);
      const refreshedShas = new Set(page.commits.map((commit) => commit.sha));
      const coversLoadedHistory = current.commits.every((commit) => refreshedShas.has(commit.sha));
      const offsets = coversLoadedHistory
        ? []
        : (loadedPageCursorsRef.current.length === 0 ? [0] : loadedPageCursorsRef.current).filter(
            (offset) => offset !== 0 || !sameTips,
          );
      // Capture the loaded pages when a fresh server response completes. This
      // snapshot stays fixed while the user loads more pages or refs catch up.
      let retained: HistoryPagesState["retainedRefresh"] = null;
      if (offsets.length > 0) {
        retainedSequenceRef.current += 1;
        retained = {
          firstPage: page,
          pinnedTips: current.pinnedTips,
          offsets,
          refreshCacheKey: historyRefreshCacheKey(viewId, retainedSequenceRef.current),
        };
      }
      next = { ...next, retainedRefresh: retained };
    }
    consumedRefreshEpochRef.current = refreshEpoch;
    pagesRef.current = next;
    setPages(next);
    if (resetCursors) {
      const currentProjectRef = {
        environmentId: projectEnvironmentId,
        projectId,
      } as ScopedProjectRef;
      const cursors =
        preserveStoredCursors && loadedPageCursorsRef.current.length > 0
          ? loadedPageCursorsRef.current
          : [0];
      setLoadedPageCursors(currentProjectRef, cursors);
      setLoadedPageCount(currentProjectRef, cursors.length);
    }
  }, [
    commitLookup,
    firstPage,
    projectEnvironmentId,
    projectId,
    refreshEpoch,
    setLoadedPageCount,
    setLoadedPageCursors,
    viewId,
  ]);

  useEffect(() => {
    if (pageLoadOffset === null || nextPageSignature === null) return;
    if (processedNextPageSignatureRef.current === nextPageSignature) return;
    const page = nextPage;
    if (page === null) return;
    processedNextPageSignatureRef.current = nextPageSignature;
    for (const commit of page.commits) commitLookup.set(commit);

    const current = pagesRef.current;
    const next: HistoryPagesState = {
      ...current,
      commits: appendUniqueCommits(current.commits, page.commits),
      nextOffset: page.nextOffset,
      exhausted: page.exhausted,
      degradedToAllPaging: current.degradedToAllPaging || page.degradedToAllPaging,
    };
    pagesRef.current = next;
    setPages(next);
    setLoadingOffset(null);
    const currentCursors = loadedPageCursorsRef.current;
    const nextCursors = currentCursors.includes(pageLoadOffset)
      ? currentCursors
      : [...currentCursors, pageLoadOffset];
    const currentProjectRef = {
      environmentId: projectEnvironmentId,
      projectId,
    } as ScopedProjectRef;
    setLoadedPageCursors(currentProjectRef, nextCursors);
    setLoadedPageCount(currentProjectRef, nextCursors.length);
  }, [
    commitLookup,
    nextPage,
    nextPageSignature,
    pageLoadOffset,
    projectEnvironmentId,
    projectId,
    setLoadedPageCount,
    setLoadedPageCursors,
  ]);

  const loadedCommitIndex = useMemo(
    () => new Map(pages.commits.map((commit) => [commit.sha, commit])),
    [pages.commits],
  );
  // A failed first page outranks a failed retained batch: its successful retry
  // captures a fresh batch. Retrying keeps the failure visible until it settles.
  const refreshFailure =
    pages.commits.length > 0 && firstPageQuery.error !== null
      ? {
          cause: firstPageQuery.error,
          retrying: firstPageQuery.emission.waiting,
          retry: refreshFirstPage,
        }
      : retainedQuery.error !== null
        ? {
            cause: retainedQuery.error,
            retrying: retainedQuery.emission.waiting,
            retry: retainedQuery.refresh,
          }
        : null;
  const selectedSha = selectedCommitSha ?? scrollAnchor;
  const selectedCommit =
    (selectedSha === null ? null : loadedCommitIndex.get(selectedSha)) ??
    (selectedSha === null ? null : commitLookup.get(selectedSha)) ??
    pages.commits[0] ??
    null;
  const effectiveSelectedSha = selectedCommit?.sha ?? null;
  const effectiveSelectedFilePath =
    selectedCommit === null
      ? null
      : selectedFilePath !== null && selectedCommit.changedFiles.includes(selectedFilePath)
        ? selectedFilePath
        : (selectedCommit.changedFiles[0] ?? null);

  const handleSelectCommit = useCallback(
    (sha: string) => {
      const commit = commitLookup.get(sha);
      const currentProjectRef = {
        environmentId: projectEnvironmentId,
        projectId,
      } as ScopedProjectRef;
      setSelectedCommit(currentProjectRef, sha);
      setMultiCommitSelection(currentProjectRef, []);
      setSelectedFile(currentProjectRef, commit?.changedFiles[0] ?? null);
      setScrollAnchor(currentProjectRef, sha);
    },
    [
      commitLookup,
      projectEnvironmentId,
      projectId,
      setScrollAnchor,
      setMultiCommitSelection,
      setSelectedCommit,
      setSelectedFile,
    ],
  );
  const handleMultiSelect = useCallback(
    (sha: string, mode: "range" | "toggle") => {
      const currentProjectRef = {
        environmentId: projectEnvironmentId,
        projectId,
      } as ScopedProjectRef;
      let nextSelection: ReadonlyArray<string>;
      if (mode === "toggle") {
        nextSelection = multiCommitSelection.includes(sha)
          ? multiCommitSelection.filter((selectedSha) => selectedSha !== sha)
          : [...multiCommitSelection, sha];
      } else {
        const anchorSha = selectedCommitSha ?? pagesRef.current.commits[0]?.sha ?? sha;
        const anchorIndex = pagesRef.current.commits.findIndex(
          (commit) => commit.sha === anchorSha,
        );
        const selectedIndex = pagesRef.current.commits.findIndex((commit) => commit.sha === sha);
        if (anchorIndex < 0 || selectedIndex < 0) {
          nextSelection = [sha];
        } else {
          const start = Math.min(anchorIndex, selectedIndex);
          const end = Math.max(anchorIndex, selectedIndex);
          nextSelection = pagesRef.current.commits
            .slice(start, end + 1)
            .map((commit) => commit.sha);
        }
      }
      setMultiCommitSelection(currentProjectRef, nextSelection);
      setSelectedCommit(currentProjectRef, sha);
      setScrollAnchor(currentProjectRef, sha);
    },
    [
      multiCommitSelection,
      projectEnvironmentId,
      projectId,
      selectedCommitSha,
      setMultiCommitSelection,
      setScrollAnchor,
      setSelectedCommit,
    ],
  );
  const orderedSelection = useCallback((shas: ReadonlyArray<string>): ReadonlyArray<string> => {
    const selected = new Set(shas);
    return pagesRef.current.commits.flatMap((commit) =>
      selected.has(commit.sha) ? [commit.sha] : [],
    );
  }, []);
  const squashMessage = useCallback((shas: ReadonlyArray<string>): string => {
    const selected = new Set(shas);
    const subjects = pagesRef.current.commits.flatMap((commit) => {
      const subject = commit.subject.trim();
      return selected.has(commit.sha) && subject.length > 0 ? [subject] : [];
    });
    return subjects.join("\n\n") || `Squash ${shas.length} commits`;
  }, []);
  const emitMenuAction = useCallback(
    (id: GitManagerCommitMenuItemId, selection: ReadonlyArray<string>) => {
      const sha = selection[0];
      if (sha === undefined) return;
      switch (id) {
        case "reset":
          onAction({ _tag: "reset", sha });
          return;
        case "revert":
          onAction({ _tag: "revert", sha });
          return;
        case "cherry-pick":
          onAction({ _tag: "cherry-pick", shas: selection });
          return;
        case "squash":
          onAction({ _tag: "squash", shas: selection, message: squashMessage(selection) });
          return;
        case "reorder":
          onAction({ _tag: "prepare-reorder", shas: selection });
          return;
        case "create-branch":
          onAction({ _tag: "create-branch", sha });
          return;
        case "create-tag":
          onAction({ _tag: "create-tag", sha });
          return;
        case "copy-sha": {
          const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
          if (clipboard?.writeText === undefined) {
            setActionError("Could not copy the commit SHA. Copy it from the commit details.");
            return;
          }
          void clipboard.writeText(selection.join("\n")).catch(() => {
            setActionError("Could not copy the commit SHA. Copy it from the commit details.");
          });
        }
      }
    },
    [onAction, squashMessage],
  );
  const handleContextMenu = useCallback(
    (sha: string, event: MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      const selection = multiCommitSelection.includes(sha) ? multiCommitSelection : [sha];
      if (!multiCommitSelection.includes(sha)) {
        setMultiCommitSelection(projectRef, selection);
      }
      const api = readLocalApi();
      if (api === undefined) {
        setActionError("The commit action menu is unavailable. Try reopening Git Manager.");
        return;
      }
      setActionError(null);
      const items: ReadonlyArray<ContextMenuItem<GitManagerCommitMenuItemId>> =
        buildCommitMenuItems(selection, {
          loadedCommits: pagesRef.current.commits,
          blockedReasons,
          branchSyncDisabledReason,
          rewriteDisabledReason,
          tagDisabledReason,
        }).map((menuItem) => ({
          id: menuItem.id,
          label:
            menuItem.disabledReason === null
              ? menuItem.label
              : `${menuItem.label} — ${menuItem.disabledReason}`,
          disabled: !menuItem.enabled,
          destructive: menuItem.id === "reset",
        }));
      void api.contextMenu
        .show(items, { x: event.clientX, y: event.clientY })
        .then((action) => {
          if (action !== null) emitMenuAction(action, selection);
        })
        .catch(() => {
          setActionError("The commit action menu could not open. Try the action again.");
        });
    },
    [
      blockedReasons,
      branchSyncDisabledReason,
      emitMenuAction,
      multiCommitSelection,
      projectRef,
      rewriteDisabledReason,
      setMultiCommitSelection,
      tagDisabledReason,
    ],
  );
  const handleCommitDrop = useCallback(
    (resolution: GitManagerCommitDropResolution) => {
      setActionError(null);
      if (rewriteDisabledReason !== null) {
        setActionError(rewriteDisabledReason);
        return;
      }
      switch (resolution._tag) {
        case "blocked":
          setActionError(resolution.reason.message);
          return;
        case "cherry-pick":
          onAction({ _tag: "cherry-pick", shas: resolution.shas });
          return;
        case "reorder":
          onAction({
            _tag: "reorder",
            shas: resolution.shas,
            insertBeforeSha: resolution.insertBeforeSha,
          });
          return;
        case "squash": {
          const shas = orderedSelection([resolution.targetSha, ...resolution.shas]);
          onAction({ _tag: "squash", shas, message: squashMessage(shas) });
        }
      }
    },
    [onAction, orderedSelection, rewriteDisabledReason, squashMessage],
  );
  const handleSelectFile = useCallback(
    (path: string) =>
      setSelectedFile({ environmentId: projectEnvironmentId, projectId } as ScopedProjectRef, path),
    [projectEnvironmentId, projectId, setSelectedFile],
  );
  const handleReachEnd = useCallback(() => {
    if (pageLoadOffset !== null) {
      // Reaching the end again is how the user retries a failed page.
      if (loadMoreError !== null) retryNextPage();
      return;
    }
    const current = pagesRef.current;
    if (current.exhausted || current.nextOffset === null) return;
    setLoadingOffset(current.nextOffset);
  }, [loadMoreError, pageLoadOffset, retryNextPage]);
  const handleRefresh = useCallback(() => {
    commitLookup.clear();
    const reset = emptyHistoryPages();
    pagesRef.current = reset;
    processedNextPageSignatureRef.current = null;
    handledTipsResetRequestRef.current = null;
    setPages(reset);
    setLoadingOffset(null);
    setTipsResetRequest(null);
    const currentProjectRef = {
      environmentId: projectEnvironmentId,
      projectId,
    } as ScopedProjectRef;
    setSelectedCommit(currentProjectRef, null);
    setSelectedFile(currentProjectRef, null);
    setScrollAnchor(currentProjectRef, null);
    setLoadedPageCursors(currentProjectRef, []);
    setLoadedPageCount(currentProjectRef, 0);
    setRefreshEpoch((epoch) => epoch + 1);
    refreshFirstPage();
  }, [
    commitLookup,
    refreshFirstPage,
    projectEnvironmentId,
    projectId,
    setLoadedPageCount,
    setLoadedPageCursors,
    setScrollAnchor,
    setSelectedCommit,
    setSelectedFile,
  ]);

  if ((firstPageAtom === null || firstPageQuery.isPending) && pages.commits.length === 0) {
    return (
      <p role="status" className="p-4 text-sm text-muted-foreground">
        Loading commit history…
      </p>
    );
  }
  if (firstPageQuery.error !== null && pages.commits.length === 0) {
    return (
      <div className="space-y-2 p-4">
        <p className="text-sm text-destructive">Couldn’t load history: {firstPageQuery.error}</p>
        <Button size="xs" variant="outline" onClick={handleRefresh}>
          Retry
        </Button>
      </div>
    );
  }

  return (
    <section aria-label="Repository history" className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-3 py-2">
        <div>
          <h1 className="text-balance text-xs font-semibold">History</h1>
          <p className="text-xs text-muted-foreground tabular-nums">
            {pages.commits.length} commit{pages.commits.length === 1 ? "" : "s"} loaded
          </p>
        </div>
        <button
          type="button"
          aria-label="Refresh history"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
          onClick={handleRefresh}
        >
          <RefreshCwIcon aria-hidden="true" className="size-3.5" />
        </button>
      </div>
      {pages.degradedToAllPaging ? (
        <p
          role="status"
          className="shrink-0 border-b border-warning/30 bg-warning/10 px-3 py-2 text-xs"
        >
          History may shift while new commits arrive because this repository exceeds the pinned-tip
          limit.
        </p>
      ) : null}
      {refreshFailure === null ? null : (
        <div
          role="status"
          className="flex shrink-0 items-center gap-2 border-b border-destructive/30 px-3 py-2 text-xs text-destructive"
        >
          <span className="min-w-0 flex-1">
            Couldn’t refresh history: {asSentence(refreshFailure.cause)} Your loaded commits are
            still available.
          </span>
          <RetryButton retrying={refreshFailure.retrying} onRetry={refreshFailure.retry} />
        </div>
      )}
      {loadMoreError !== null ? (
        <p
          role="status"
          className="shrink-0 border-b border-destructive/30 px-3 py-2 text-xs text-destructive"
        >
          {loadMoreError}
        </p>
      ) : null}
      {actionError === null ? null : (
        <p
          aria-live="polite"
          className="shrink-0 border-b border-destructive/30 px-3 py-2 text-xs text-destructive"
        >
          {actionError}
        </p>
      )}
      {pages.commits.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">This repository has no commits yet.</p>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-[minmax(220px,34%)_minmax(0,1fr)] overflow-hidden rounded-lg border border-border/70">
          <GitManagerCommitList
            commits={pages.commits}
            rewriteDisabledReason={rewriteDisabledReason}
            selectedSha={effectiveSelectedSha}
            onSelect={handleSelectCommit}
            onReachEnd={handleReachEnd}
            isLoadingMore={isLoadingMore}
            multiCommitSelection={multiCommitSelection}
            onCommitDrop={handleCommitDrop}
            onContextMenu={handleContextMenu}
            onMultiSelect={handleMultiSelect}
          />
          {selectedCommit === null ? (
            <p className="p-4 text-sm text-muted-foreground">Select a commit to inspect it.</p>
          ) : (
            <GitManagerCommitDetail
              environmentId={environmentId}
              cwd={cwd}
              commit={selectedCommit}
              selectedFilePath={effectiveSelectedFilePath}
              onSelectFile={handleSelectFile}
            />
          )}
        </div>
      )}
    </section>
  );
});
