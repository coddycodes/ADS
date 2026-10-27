import {
  resolveChangeRequestPresentationForKind,
  formatChangeRequestNumber,
} from "@coddy/shared/sourceControl";
import { useAtomRefresh } from "@effect/atom-react";
import { projectKey } from "@coddy/client-runtime/state/entities";
import type {
  EnvironmentId,
  PullRequestsChecks as PullRequestsChecksData,
  PullRequestsCommits as PullRequestsCommitsData,
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsFiles as PullRequestsFilesData,
  PullRequestsTimeline,
  ScopedProjectRef,
} from "@coddy/contracts";
import {
  lazy,
  type ReactNode,
  Suspense,
  useCallback,
  useEffect,
  useEffectEvent,
  useMemo,
  useState,
} from "react";
import { PullRequestsActionProvider } from "../usePullRequestsAction";
import { usePullRequestsStore, type PullRequestsDetailTab } from "../../../pullRequestsStore";
import { pullRequestsEnvironment } from "../../../state/pullRequests";
import { useEnvironmentQuery, type EnvironmentQueryView } from "../../../state/query";
import { Tabs, TabsList, TabsPanel, TabsTab } from "../../ui/tabs";
import { Button } from "../../ui/button";
import { usePullRequestsQuery } from "../shared/usePullRequestsQuery";
import { prefetchedWithin } from "../hoverPrefetch.logic";
import { useDocumentVisible } from "../useDocumentVisible";
import { useVisiblePullRequestRefresh } from "../useVisiblePullRequestRefresh";
import {
  buildListInput,
  selectListFilters,
  selectListSort,
  selectListTab,
} from "../list/pullRequestsList.logic";
import { newerObservedAt } from "./snapshotDisplay.logic";
import { PullRequestsQueryState } from "./PullRequestsQueryState";
import { PullRequestsHeader } from "./PullRequestsHeader";
import { PullRequestsConversation } from "./PullRequestsConversation";
import { PullRequestsSideColumn } from "./PullRequestsSideColumn";
import { PullRequestsCommits } from "./PullRequestsCommits";
import { PullRequestsChecks } from "./PullRequestsChecks";
import { usePullRequestsNavigation } from "./pullRequestsNavigation";
const PullRequestsFiles = lazy(() =>
  import("./PullRequestsFiles").then((module) => ({ default: module.PullRequestsFiles })),
);
export interface PullRequestsDetailViewProps {
  scope: { environmentId: EnvironmentId; cwd: string };
  projectRef: ScopedProjectRef;
  context: Extract<PullRequestsContext, { status: "available" }>;
  number: number;
  tab: PullRequestsDetailTab;
  mutationsDisabledReason?: string | null;
  /** Extra actions in the top row, given the loaded detail (the panel's "Open full page"). */
  headerActions?: ((detail: PullRequestsDetail) => ReactNode) | undefined;
}
export function PullRequestsDetailView({
  scope,
  projectRef,
  context,
  number,
  tab,
  mutationsDisabledReason = null,
  headerActions,
}: PullRequestsDetailViewProps) {
  const navigation = usePullRequestsNavigation();
  const request = useMemo(
    () => ({ environmentId: scope.environmentId, input: { cwd: scope.cwd, number } }),
    [number, scope.cwd, scope.environmentId],
  );
  // A hover prefetch that answered within FRESH_MS is shown as is on open.
  const [prefetched] = useState(() => ({
    detail: prefetchedWithin("get", request, Date.now()),
    timeline: prefetchedWithin("getTimeline", request, Date.now()),
  }));
  const detailQuery = usePullRequestsQuery(
    useMemo(() => pullRequestsEnvironment.get(request), [request]),
    { freshOnOpen: prefetched.detail },
  );
  const timelineAtom = useMemo(() => pullRequestsEnvironment.getTimeline(request), [request]);
  const commitsAtom = useMemo(() => pullRequestsEnvironment.getCommits(request), [request]);
  const filesAtom = useMemo(() => pullRequestsEnvironment.getFiles(request), [request]);
  const checksAtom = useMemo(() => pullRequestsEnvironment.getChecks(request), [request]);
  const timelineQuery = usePullRequestsQuery(
    tab === "conversation" || tab === "files" ? timelineAtom : null,
    { freshOnOpen: prefetched.timeline },
  );
  const commitsQuery = usePullRequestsQuery(tab === "commits" ? commitsAtom : null);
  const checksQuery = usePullRequestsQuery(tab === "checks" ? checksAtom : null);
  const filesQuery = usePullRequestsQuery(tab === "files" ? filesAtom : null);
  // Refreshing an inactive atom invalidates its cache without mounting another query.
  const refreshTimelineAtom = useAtomRefresh(timelineAtom);
  const refreshCommitsAtom = useAtomRefresh(commitsAtom);
  const refreshFilesAtom = useAtomRefresh(filesAtom);
  const refreshChecksAtom = useAtomRefresh(checksAtom);
  const navigateAfterAction = useCallback(
    (target: number | null) => {
      if (target === null) navigation.requestGone();
      else navigation.openRequest(target);
    },
    [navigation],
  );
  // Action receipts re-read automatically, so they keep an exhausted cut-off latched;
  // a latched query keeps its own failure card with an explicit Retry.
  const actionRefresh = useMemo(
    () => ({
      get: detailQuery.revalidate,
      timeline:
        tab === "conversation" || tab === "files" ? timelineQuery.revalidate : refreshTimelineAtom,
      files: tab === "files" ? filesQuery.revalidate : refreshFilesAtom,
      commits: tab === "commits" ? commitsQuery.revalidate : refreshCommitsAtom,
      checks: tab === "checks" ? checksQuery.revalidate : refreshChecksAtom,
      navigate: navigateAfterAction,
      discardDeleted: navigation.discardDeleted ?? undefined,
    }),
    [
      detailQuery.revalidate,
      tab,
      timelineQuery.revalidate,
      filesQuery.revalidate,
      commitsQuery.revalidate,
      refreshTimelineAtom,
      refreshFilesAtom,
      refreshCommitsAtom,
      checksQuery.revalidate,
      refreshChecksAtom,
      navigateAfterAction,
      navigation.discardDeleted,
    ],
  );
  const activeQuery = {
    conversation: timelineQuery,
    commits: commitsQuery,
    checks: checksQuery,
    files: filesQuery,
  }[tab];
  const isGitlab = context.provider === "gitlab";
  // The same list the project's list view would show, so this detail's poller
  // joins that list's background-sync group instead of starting another one.
  const key = projectKey(projectRef);
  const listFilters = usePullRequestsStore((state) => selectListFilters(state.byProjectKey[key]));
  const listTab = usePullRequestsStore((state) => selectListTab(state.byProjectKey[key]));
  const listSort = usePullRequestsStore((state) => selectListSort(state.byProjectKey[key]));
  const listInput = useMemo(
    () => buildListInput({ filters: listFilters, listTab, sort: listSort }, scope.cwd),
    [listFilters, listTab, listSort, scope.cwd],
  );
  const snapshotTarget = useMemo(
    () => ({ environmentId: scope.environmentId, input: { ...listInput, number, tab } }),
    [listInput, number, scope.environmentId, tab],
  );
  // Both hosts paint stored copies; only GitLab subscribes to changes.
  const snapshotAtom = useMemo(
    () => pullRequestsEnvironment.readSnapshot(snapshotTarget),
    [snapshotTarget],
  );
  const snapshotQuery = useEnvironmentQuery(snapshotAtom);
  const visible = useDocumentVisible();
  const subscribeAtom = useMemo(
    () => (isGitlab && visible ? pullRequestsEnvironment.subscribe(snapshotTarget) : null),
    [isGitlab, snapshotTarget, visible],
  );
  const subscribeQuery = useEnvironmentQuery(subscribeAtom);
  // A success on the push stream, even the initial all-false connected event,
  // means the server is actively watching this request; the client's own
  // timer stands down until the stream drops back to waiting or fails.
  const paused = isGitlab && subscribeQuery.emission._tag === "Success";
  const [paintedDetail, setPaintedDetail] = useState<{
    payload: PullRequestsDetail;
    observedAt: number;
  } | null>(null);
  const snapshotDetail = snapshotQuery.data?.detail ?? null;
  if (
    snapshotDetail !== null &&
    newerObservedAt(paintedDetail?.observedAt ?? null, snapshotDetail.observedAt)
  ) {
    setPaintedDetail(snapshotDetail);
  }
  const [paintedTab, setPaintedTab] = useState<{
    tab: PullRequestsDetailTab;
    payload:
      | PullRequestsTimeline
      | PullRequestsCommitsData
      | PullRequestsChecksData
      | PullRequestsFilesData;
    observedAt: number;
  } | null>(null);
  const snapshotTab = snapshotQuery.data?.tab ?? null;
  if (
    snapshotTab !== null &&
    (paintedTab === null ||
      paintedTab.tab !== snapshotTab.kind ||
      newerObservedAt(paintedTab.observedAt, snapshotTab.observedAt))
  ) {
    setPaintedTab({
      tab: snapshotTab.kind,
      payload: snapshotTab.payload,
      observedAt: snapshotTab.observedAt,
    });
  }
  const applySubscribedChange = useEffectEvent(
    (changed: NonNullable<typeof subscribeQuery.data>) => {
      if (changed.detail) detailQuery.revalidate();
      if (changed.timeline)
        (tab === "conversation" || tab === "files"
          ? timelineQuery.revalidate
          : refreshTimelineAtom)();
      if (changed.commits) (tab === "commits" ? commitsQuery.revalidate : refreshCommitsAtom)();
      if (changed.checks) (tab === "checks" ? checksQuery.revalidate : refreshChecksAtom)();
      if (changed.files) (tab === "files" ? filesQuery.revalidate : refreshFilesAtom)();
    },
  );
  useEffect(() => {
    if (subscribeQuery.data) applySubscribedChange(subscribeQuery.data);
  }, [subscribeQuery.data]);
  const detailSucceeded =
    detailQuery.data !== null && detailQuery.error === null && !detailQuery.isPending;
  // Action controls (merge, review, side column, header) act only on this live
  // detail, never a snapshot. Unlike `detailSucceeded` above (which gates the
  // visible-refresh timer), this stays set through a revalidate's `isPending`
  // window: the atom still holds its own live value then, just a stale one.
  const liveDetail =
    detailQuery.data !== null && detailQuery.error === null ? detailQuery.data : null;
  const displayedDetailData = detailQuery.data ?? paintedDetail?.payload ?? null;
  const displayedDetailQuery: EnvironmentQueryView<PullRequestsDetail> =
    detailQuery.data !== null || paintedDetail === null
      ? detailQuery
      : { ...detailQuery, data: paintedDetail.payload, isPending: false };
  const paintedActivePayload = paintedTab?.tab === tab ? paintedTab.payload : null;
  const displayedTimeline =
    timelineQuery.data ??
    (tab === "conversation" && paintedActivePayload
      ? (paintedActivePayload as PullRequestsTimeline)
      : null);
  const displayedCommits =
    commitsQuery.data ??
    (tab === "commits" && paintedActivePayload
      ? (paintedActivePayload as PullRequestsCommitsData)
      : null);
  const displayedChecks =
    checksQuery.data ??
    (tab === "checks" && paintedActivePayload
      ? (paintedActivePayload as PullRequestsChecksData)
      : null);
  const displayedFiles =
    filesQuery.data ??
    (tab === "files" && paintedActivePayload
      ? (paintedActivePayload as PullRequestsFilesData)
      : null);
  const displayedActiveData = {
    conversation: displayedTimeline,
    commits: displayedCommits,
    checks: displayedChecks,
    files: displayedFiles,
  }[tab];
  const displayedActiveQuery =
    activeQuery.data !== null || displayedActiveData === null
      ? activeQuery
      : { ...activeQuery, data: displayedActiveData, isPending: false };
  useVisiblePullRequestRefresh({
    enabled: isGitlab,
    succeeded: detailSucceeded,
    paused,
    revalidate: detailQuery.revalidate,
  });
  useVisiblePullRequestRefresh({
    enabled: isGitlab && activeQuery.data !== null,
    succeeded: activeQuery.data !== null && activeQuery.error === null && !activeQuery.isPending,
    paused,
    revalidate: activeQuery.revalidate,
  });
  useEffect(() => {
    usePullRequestsStore.getState().setLastNumber(projectRef, number);
  }, [number, projectRef]);
  const vocabulary = context.capabilities.vocabulary;
  const presentation = resolveChangeRequestPresentationForKind(context.provider);
  const labels = {
    conversation: "Conversation",
    commits: "Commits",
    checks: vocabulary.checks,
    files: vocabulary.filesChanged,
  };
  const detail = displayedDetailData;
  const refreshing = detailQuery.isPending || activeQuery.isPending;
  const refreshDetail = detailQuery.refresh;
  const refreshTab = activeQuery.refresh;
  const refreshTimeline = timelineQuery.refresh;
  const refresh = useCallback(() => {
    refreshDetail();
    refreshTab();
    if (tab === "files") refreshTimeline();
  }, [refreshDetail, refreshTab, tab, refreshTimeline]);
  return (
    <PullRequestsActionProvider
      key={JSON.stringify([scope.environmentId, scope.cwd, number])}
      scope={scope}
      number={number}
      requestKind={presentation.longName}
      refresh={actionRefresh}
      disabledReason={mutationsDisabledReason}
    >
      <div
        className="@container/pull-request flex min-h-0 min-w-0 flex-1 flex-col"
        data-text-surface="background"
      >
        {navigation.back !== null || (headerActions && detail) ? (
          <div className="flex shrink-0 items-center justify-between gap-2 px-4 pt-2">
            {navigation.back !== null ? (
              <Button size="sm" variant="link" onClick={navigation.back}>
                Back to {presentation.pluralLongName}
              </Button>
            ) : (
              <span />
            )}
            {headerActions && detail ? headerActions(detail) : null}
          </div>
        ) : null}
        <PullRequestsQueryState
          query={displayedDetailQuery}
          label={`${presentation.longName} ${formatChangeRequestNumber(context.provider, number)}`}
        >
          {detail ? (
            <>
              <PullRequestsHeader
                scope={scope}
                projectRef={projectRef}
                detail={detail}
                live={liveDetail}
                context={context}
                onRefresh={refresh}
                refreshing={refreshing}
              />
              <Tabs
                value={tab}
                className="min-h-0 flex-1 gap-0"
                onValueChange={(value) => {
                  if (
                    value === "conversation" ||
                    value === "commits" ||
                    value === "checks" ||
                    value === "files"
                  )
                    navigation.selectTab(value);
                }}
              >
                <TabsList
                  aria-label={`${presentation.longName} sections`}
                  className="mx-4 max-w-full shrink-0 overflow-x-auto"
                >
                  {(Object.keys(labels) as PullRequestsDetailTab[]).map((value) => (
                    <TabsTab key={value} value={value}>
                      {labels[value]}
                      {detail.tabCounts[value] === null ? "" : ` ${detail.tabCounts[value]}`}
                    </TabsTab>
                  ))}
                </TabsList>
                <div className="flex min-h-0 flex-1">
                  <TabsPanel value={tab} className="min-h-0 flex-1 gap-0">
                    <PullRequestsQueryState
                      key={tab}
                      query={displayedActiveQuery}
                      label={labels[tab]}
                    >
                      {tab === "conversation" && displayedTimeline ? (
                        <PullRequestsConversation
                          scope={scope}
                          detail={detail}
                          context={context}
                          projectRef={projectRef}
                          timeline={displayedTimeline}
                          detailsRefreshing={detailQuery.isPending}
                          liveDetail={liveDetail}
                        />
                      ) : tab === "commits" && displayedCommits ? (
                        <PullRequestsCommits commits={displayedCommits} />
                      ) : tab === "checks" && displayedChecks ? (
                        <PullRequestsChecks checks={displayedChecks} context={context} />
                      ) : tab === "files" && displayedFiles ? (
                        <Suspense
                          fallback={
                            <p role="status" className="p-4 text-sm">
                              Preparing diffs…
                            </p>
                          }
                        >
                          <PullRequestsQueryState query={timelineQuery} label="Review threads">
                            {displayedTimeline ? (
                              <PullRequestsFiles
                                timeline={displayedTimeline}
                                files={displayedFiles}
                                detail={detail}
                                live={liveDetail}
                                projectRef={projectRef}
                                context={context}
                              />
                            ) : null}
                          </PullRequestsQueryState>
                        </Suspense>
                      ) : null}
                    </PullRequestsQueryState>
                  </TabsPanel>
                  <div
                    data-pull-request-side-column="wide"
                    className="hidden w-64 shrink-0 overflow-auto border-l border-border @3xl/pull-request:block"
                  >
                    <PullRequestsSideColumn
                      detail={detail}
                      live={liveDetail}
                      context={context}
                      scope={scope}
                      projectRef={projectRef}
                      detailsRefreshing={detailQuery.isPending}
                    />
                  </div>
                </div>
              </Tabs>
            </>
          ) : null}
        </PullRequestsQueryState>
      </div>
    </PullRequestsActionProvider>
  );
}
