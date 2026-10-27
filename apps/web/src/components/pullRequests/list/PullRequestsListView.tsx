import { resolveChangeRequestPresentationForKind } from "@coddy/shared/sourceControl";
import { projectKey } from "@coddy/client-runtime/state/entities";
import { environmentRpcKey, squashAtomCommandFailure } from "@coddy/client-runtime/state/runtime";
import {
  PullRequestsOperationError,
  type EnvironmentId,
  type PullRequestsContext,
  type PullRequestsListInput,
  type PullRequestsListPage,
  type PullRequestsListRow,
  type ScopedProjectRef,
} from "@coddy/contracts";
import * as Schema from "effect/Schema";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import {
  useCallback,
  useContext,
  useEffect,
  useEffectEvent,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type Ref,
} from "react";
import { DEFAULT_PULL_REQUESTS_FILTERS, usePullRequestsStore } from "../../../pullRequestsStore";
import { pullRequestsEnvironment } from "../../../state/pullRequests";
import { useEnvironmentQuery, type EnvironmentQueryView } from "../../../state/query";
import { Button } from "../../ui/button";
import { Skeleton } from "../../ui/skeleton";
import { Tabs, TabsList, TabsPanel, TabsTab } from "../../ui/tabs";
import { PullRequestsContextRefresh } from "../pullRequestsContextRefresh";
import { usePullRequestsQuery } from "../shared/usePullRequestsQuery";
import { useVisiblePullRequestRefresh } from "../useVisiblePullRequestRefresh";
import { shownCopy, type ShownCopy } from "../detail/snapshotDisplay.logic";
import { useDocumentVisible } from "../useDocumentVisible";
import { PullRequestsFilters } from "./PullRequestsFilters";
import { PullRequestsRow } from "./PullRequestsRow";
import {
  buildListInput,
  selectListFilters,
  selectListSort,
  selectListTab,
} from "./pullRequestsList.logic";

export interface PullRequestsListViewHandle {
  refresh: () => void;
}
export interface PullRequestsListViewProps {
  scope: { environmentId: EnvironmentId; cwd: string };
  projectRef: ScopedProjectRef;
  context: Extract<PullRequestsContext, { status: "available" }>;
  ref?: Ref<PullRequestsListViewHandle> | undefined;
  /**
   * The page read alongside this context load. Rendering only reads this value;
   * the committed list acknowledges it so later opens refresh.
   */
  freshPageKey?: string | null | undefined;
  onFreshPageConsumed?: ((key: string) => void) | undefined;
}
const isPullRequestsOperationError = Schema.is(PullRequestsOperationError);
const TABS = { open: "Open", merged: "Merged", closed: "Closed", all: "All" };
const COMBINED_TABS = ["open", "closed"] as const;
const SEPARATE_TABS = ["open", "merged", "closed", "all"] as const;
const EMPTY_ROWS: readonly PullRequestsListRow[] = [];
const getRowSize = () => 56;
const rowKey = (row: PullRequestsListRow) => String(row.number);

function PullRequestsPages({
  scope,
  projectRef,
  context,
  ref,
  input,
  firstQuery,
  hasFilters,
  onClear,
}: PullRequestsListViewProps & {
  input: PullRequestsListInput;
  firstQuery: EnvironmentQueryView<PullRequestsListPage>;
  hasFilters: boolean;
  onClear: () => void;
}) {
  const [cursors, setCursors] = useState<readonly (string | null)[]>([null]);
  const [earlierRows, setEarlierRows] = useState<readonly PullRequestsListRow[]>(EMPTY_ROWS);
  const cursor = cursors.at(-1) ?? null;
  const nextAtom = useMemo(
    () =>
      cursor === null
        ? null
        : pullRequestsEnvironment.list({
            environmentId: scope.environmentId,
            input: { ...input, cursor },
          }),
    [cursor, input, scope.environmentId],
  );
  const nextQuery = usePullRequestsQuery(nextAtom);
  const query = cursor === null ? firstQuery : nextQuery;
  const rows = useMemo(() => {
    const combined =
      cursor === null
        ? (firstQuery.data?.rows ?? EMPTY_ROWS)
        : [
            ...(firstQuery.data?.rows ?? EMPTY_ROWS),
            ...earlierRows,
            ...(nextQuery.data?.rows ?? EMPTY_ROWS),
          ];
    return [...new Map(combined.map((row) => [row.number, row])).values()];
  }, [cursor, earlierRows, firstQuery.data, nextQuery.data]);
  const savedScrollTop = usePullRequestsStore(
    (state) => state.byProjectKey[projectKey(projectRef)]?.scrollTop ?? 0,
  );
  const scrollTopRef = useRef(savedScrollTop);
  const scrollRef = useRef<LegendListRef | null>(null);
  const restorationTarget = useRef(savedScrollTop > 0 ? savedScrollTop : null);
  useEffect(() => {
    const filtersOnOpen = usePullRequestsStore.getState().selectViewState(projectRef).filters;
    const persistScroll = () => {
      const store = usePullRequestsStore.getState();
      const current = store.selectViewState(projectRef);
      // Filter/checkout resets own their new scroll position, even for equal query values.
      if (
        current.filters === filtersOnOpen &&
        (current.checkoutCwd ?? scope.cwd) === scope.cwd &&
        JSON.stringify(buildListInput(current, scope.cwd)) === JSON.stringify(input)
      ) {
        store.setScrollTop(projectRef, scrollTopRef.current);
      }
    };
    window.addEventListener("pagehide", persistScroll);
    return () => {
      window.removeEventListener("pagehide", persistScroll);
      persistScroll();
    };
  }, [input, projectRef, scope.cwd]);
  const refreshFirstPage = firstQuery.refresh;
  const environmentId = scope.environmentId;
  const refresh = useCallback(() => {
    restorationTarget.current = null;
    setCursors([null]);
    setEarlierRows(EMPTY_ROWS);
    scrollTopRef.current = 0;
    usePullRequestsStore.getState().setScrollTop(projectRef, 0);
    scrollRef.current?.scrollToOffset({ offset: 0, animated: false });
    // An explicit Refresh also re-reads the repository-wide tab totals.
    pullRequestsEnvironment.requestListTotalsRefresh({ environmentId, input });
    refreshFirstPage();
  }, [environmentId, input, refreshFirstPage, projectRef]);
  useImperativeHandle(ref, () => ({ refresh }), [refresh]);
  const renderRow = useCallback(
    ({ item }: { item: PullRequestsListRow }) => (
      <PullRequestsRow row={item} projectRef={projectRef} context={context} scope={scope} />
    ),
    [context, projectRef, scope],
  );
  const failure =
    query.emission._tag === "Failure" ? squashAtomCommandFailure(query.emission) : null;
  const typedError = isPullRequestsOperationError(failure) ? failure : null;
  const refreshContext = useContext(PullRequestsContextRefresh);
  useEffect(() => {
    if (typedError?.code === "not_authenticated") refreshContext?.();
  }, [refreshContext, typedError]);
  const message = typedError?.message ?? query.error;
  const nextCursor = query.data?.nextCursor ?? null;
  const loadMore = () => {
    if (query.isPending || nextCursor === null || cursors.includes(nextCursor)) return;
    if (cursor !== null && nextQuery.data) {
      const pageRows = nextQuery.data.rows;
      setEarlierRows((previous) => [...previous, ...pageRows]);
    }
    setCursors((previous) => [...previous, nextCursor]);
  };
  const restorePagePosition = useEffectEvent(
    (
      pageData: PullRequestsListPage | null,
      pending: boolean,
      error: string | null,
      rowCount: number,
      next: string | null,
    ) => {
      const target = restorationTarget.current;
      if (target === null || pending || pageData === null) return;
      if (error !== null) {
        restorationTarget.current = null;
        return;
      }
      const viewportHeight = scrollRef.current?.getScrollableNode().clientHeight ?? 0;
      if (rowCount * 56 <= target + viewportHeight && next !== null && !cursors.includes(next)) {
        loadMore();
        return;
      }
      scrollRef.current?.scrollToOffset({ offset: target, animated: false });
      restorationTarget.current = null;
    },
  );
  useEffect(() => {
    restorePagePosition(query.data, query.isPending, message, rows.length, nextCursor);
  }, [message, nextCursor, query.data, query.isPending, rows.length]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {message !== null ? (
        <div role="alert" className="space-y-2 border-b border-border p-4 text-sm">
          <p>{message}</p>
          {typedError?.hostDetail ? (
            <p className="text-xs text-muted-foreground">{typedError.hostDetail}</p>
          ) : null}
          <div className="flex gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={query.refresh}
              disabled={query.isPending}
              title={query.isPending ? "Loading…" : undefined}
            >
              Retry
            </Button>
            {hasFilters ? (
              <Button size="sm" variant="outline" onClick={onClear}>
                Clear filters
              </Button>
            ) : null}
          </div>
        </div>
      ) : null}
      {rows.length > 0 ? (
        <LegendList<PullRequestsListRow>
          ref={scrollRef}
          className="min-h-0 flex-1 overflow-x-hidden"
          role="list"
          aria-label={resolveChangeRequestPresentationForKind(context.provider).pluralLongName}
          data={rows}
          estimatedItemSize={56}
          getFixedItemSize={getRowSize}
          keyExtractor={rowKey}
          initialScrollOffset={savedScrollTop}
          renderItem={renderRow}
          onWheel={() => {
            restorationTarget.current = null;
          }}
          onTouchMove={() => {
            restorationTarget.current = null;
          }}
          onScroll={(event) => {
            scrollTopRef.current = event.nativeEvent.contentOffset.y;
          }}
        />
      ) : null}
      {query.isPending ? (
        <div role="status" className="space-y-2 p-4">
          <span className="sr-only">
            Loading {resolveChangeRequestPresentationForKind(context.provider).pluralLongName}…
          </span>
          <Skeleton className="h-14 w-full" />
        </div>
      ) : null}
      {rows.length === 0 && !query.isPending && message === null && query.data !== null ? (
        <div className="space-y-3 p-6 text-sm text-muted-foreground">
          <p>
            {hasFilters
              ? "Nothing matches these filters"
              : `No ${input.state === "all" ? "" : `${input.state} `}${resolveChangeRequestPresentationForKind(context.provider).pluralLongName}`}
          </p>
          {hasFilters ? (
            <Button size="sm" variant="outline" onClick={onClear}>
              Clear filters
            </Button>
          ) : null}
        </div>
      ) : null}
      {nextCursor !== null && !cursors.includes(nextCursor) && message === null ? (
        <div className="border-t border-border p-3">
          <Button
            size="sm"
            variant="outline"
            disabled={query.isPending}
            title={query.isPending ? "Loading…" : undefined}
            onClick={loadMore}
          >
            Load more
          </Button>
        </div>
      ) : null}
    </div>
  );
}
export function PullRequestsListView({
  scope,
  projectRef,
  context,
  ref,
  freshPageKey,
  onFreshPageConsumed,
}: PullRequestsListViewProps) {
  const key = projectKey(projectRef);
  const filters = usePullRequestsStore((state) => selectListFilters(state.byProjectKey[key]));
  const storedTab = usePullRequestsStore((state) => selectListTab(state.byProjectKey[key]));
  const sort = usePullRequestsStore((state) => selectListSort(state.byProjectKey[key]));
  const listTab =
    context.capabilities.closedTabIncludesMerged && (storedTab === "merged" || storedTab === "all")
      ? "closed"
      : storedTab;
  const input = useMemo(
    () => buildListInput({ filters, listTab, sort }, scope.cwd),
    [filters, listTab, scope.cwd, sort],
  );
  const firstAtom = useMemo(
    () => pullRequestsEnvironment.list({ environmentId: scope.environmentId, input }),
    [input, scope.environmentId],
  );
  const firstQuery = usePullRequestsQuery(firstAtom, {
    freshOnOpen: freshPageKey === environmentRpcKey({ environmentId: scope.environmentId, input }),
  });
  const isGitlab = context.provider === "gitlab";
  const snapshotTarget = useMemo(
    () => ({ environmentId: scope.environmentId, input: { ...input, number: null, tab: null } }),
    [input, scope.environmentId],
  );
  // Both hosts paint the stored first page; only GitLab subscribes to changes.
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
  // stands the client's own refresh timer down until the stream fails or waits again.
  const paused = isGitlab && subscribeQuery.emission._tag === "Success";
  const [shownList, setShownList] = useState<ShownCopy<PullRequestsListPage> | null>(null);
  const nextShownList = shownCopy(
    shownList,
    environmentRpcKey(snapshotTarget),
    firstQuery.data,
    // GitHub never polls, so once its live page answers a stored copy only covered the
    // wait: a late one must not replace it, whatever the two clocks say.
    isGitlab || firstQuery.data === null
      ? { answered: snapshotQuery.data !== null, row: snapshotQuery.data?.list ?? null }
      : null,
  );
  if (nextShownList !== shownList) setShownList(nextShownList);
  const applySubscribedChange = useEffectEvent(
    (changed: NonNullable<typeof subscribeQuery.data>) => {
      if (changed.list) snapshotQuery.revalidate();
    },
  );
  useEffect(() => {
    if (subscribeQuery.data) applySubscribedChange(subscribeQuery.data);
  }, [subscribeQuery.data]);
  useVisiblePullRequestRefresh({
    enabled: isGitlab,
    succeeded: firstQuery.data !== null && firstQuery.error === null && !firstQuery.isPending,
    paused,
    revalidate: firstQuery.revalidate,
  });
  const displayedFirstPage = nextShownList?.payload ?? null;
  const displayedFirstQuery: EnvironmentQueryView<PullRequestsListPage> =
    displayedFirstPage === null
      ? firstQuery
      : {
          ...firstQuery,
          data: displayedFirstPage,
          isPending: firstQuery.data === null ? false : firstQuery.isPending,
        };
  useEffect(() => {
    if (freshPageKey != null) onFreshPageConsumed?.(freshPageKey);
  }, [freshPageKey, onFreshPageConsumed]);
  const tabs = context.capabilities.closedTabIncludesMerged ? COMBINED_TABS : SEPARATE_TABS;
  const hasFilters = Object.values(filters).some((value) =>
    Array.isArray(value) ? value.length > 0 : value !== null && value !== "",
  );
  const [resetKey, setResetKey] = useState(0);
  const clear = () => {
    usePullRequestsStore.getState().setFilters(projectRef, DEFAULT_PULL_REQUESTS_FILTERS);
    setResetKey((value) => value + 1);
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Tabs
        className="min-h-0 flex-1 gap-0"
        value={listTab}
        onValueChange={(value) => {
          if (value === "open" || value === "closed" || value === "merged" || value === "all")
            usePullRequestsStore.getState().setListTab(projectRef, value);
        }}
      >
        <TabsList
          aria-label={`${resolveChangeRequestPresentationForKind(context.provider).longName} state`}
          className="mx-4 mt-3 shrink-0"
        >
          {tabs.map((tab) => {
            const counts = displayedFirstPage?.counts;
            const count =
              tab === "all"
                ? counts?.open !== null &&
                  counts?.open !== undefined &&
                  counts.closed !== null &&
                  counts.merged !== null
                  ? counts.open + counts.closed + counts.merged
                  : null
                : (counts?.[tab] ?? null);
            return (
              <TabsTab key={tab} value={tab}>
                {TABS[tab]}
                {count !== null ? ` ${count}` : ""}
              </TabsTab>
            );
          })}
        </TabsList>
        <TabsPanel value={listTab} className="min-h-0 flex-1 gap-0">
          <PullRequestsFilters
            scope={scope}
            context={context}
            filters={filters}
            sort={sort}
            resetKey={resetKey}
            onFiltersChange={(patch) =>
              usePullRequestsStore.getState().setFilters(projectRef, patch)
            }
            onSortChange={(value) => usePullRequestsStore.getState().setSort(projectRef, value)}
            onClear={clear}
          />
          <PullRequestsPages
            key={JSON.stringify([scope.environmentId, input, resetKey])}
            scope={scope}
            projectRef={projectRef}
            context={context}
            ref={ref}
            input={input}
            firstQuery={displayedFirstQuery}
            hasFilters={hasFilters}
            onClear={clear}
          />
        </TabsPanel>
      </Tabs>
    </div>
  );
}
