import {
  ActivityDetailPage as ActivityDetailPageSchema,
  ActivityRosterPage as ActivityRosterPageSchema,
  type ActivityRecordId,
  type ActivityRecordKind,
  type ActivityRecordSummary,
  type ActivitySnapshot,
} from "@bibcode/contracts";
import type { TimestampFormat } from "@bibcode/contracts/settings";
import { RefreshCwIcon, TriangleAlertIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { ActivityRightPanelSurface } from "~/rightPanelStore";
import { Alert, AlertAction, AlertDescription } from "~/components/ui/alert";
import { Button } from "~/components/ui/button";
import { ScrollArea } from "~/components/ui/scroll-area";
import { ActivityRecordDetail } from "./ActivityRecordDetail";
import { ActivityRoster, reconcileActivityRosterRecords } from "./ActivityRoster";

export type ActivityPanelRoute = Pick<
  ActivityRightPanelSurface,
  "section" | "selectedRecordKind" | "selectedRecordId"
>;

export type ActivityRosterPageData = typeof ActivityRosterPageSchema.Type;
export type ActivityDetailPageData = typeof ActivityDetailPageSchema.Type;

export interface ActivityQueryResult<Page> {
  readonly pages: ReadonlyArray<Page>;
  readonly loading: boolean;
  readonly error: string | null;
}

export interface ActivityDetailQueryResult extends ActivityQueryResult<ActivityDetailPageData> {
  readonly recordKind: ActivityRecordKind;
  readonly recordId: ActivityRecordId;
  readonly removed?: boolean;
}

export interface ActivityPanelProps {
  readonly timestampFormat: TimestampFormat;
  readonly route: ActivityPanelRoute;
  readonly snapshot: ActivitySnapshot;
  readonly roster: {
    readonly active: ActivityQueryResult<ActivityRosterPageData>;
    readonly done: ActivityQueryResult<ActivityRosterPageData>;
  };
  readonly detail: ActivityDetailQueryResult | null;
  readonly onNavigate: (route: ActivityPanelRoute) => void;
  readonly onLoadMoreRoster: (bucket: "active" | "done") => void;
  readonly onLoadMoreDetail: () => void;
  readonly onRefreshSnapshot: () => void;
  readonly onCancelActor?: (actorId: ActivityRecordId, controlRevision: number) => void;
  readonly onRetryCancellation?: (rootActorId: ActivityRecordId, operationRevision: number) => void;
  readonly cancellationError?: string | null;
  readonly now?: string;
}

function detailMatchesRoute(
  detail: ActivityDetailQueryResult | null,
  route: ActivityPanelRoute,
): detail is ActivityDetailQueryResult {
  if (
    detail === null ||
    route.selectedRecordKind === null ||
    route.selectedRecordId === null ||
    detail.recordKind !== route.selectedRecordKind ||
    detail.recordId !== route.selectedRecordId
  ) {
    return false;
  }
  return detail.pages.every(
    (page) => page.record._tag === detail.recordKind && page.record.id === detail.recordId,
  );
}

function loadingDetailForRoute(route: ActivityPanelRoute): ActivityDetailQueryResult | null {
  if (route.selectedRecordKind === null || route.selectedRecordId === null) {
    return null;
  }
  return {
    recordKind: route.selectedRecordKind,
    recordId: route.selectedRecordId as ActivityRecordId,
    pages: [],
    loading: true,
    error: null,
  };
}

function RetryBanner({
  message,
  stale = false,
  onRetry,
}: {
  readonly message: string;
  readonly stale?: boolean;
  readonly onRetry?: () => void;
}) {
  return (
    <Alert className="mx-3 mt-3" variant={stale ? "warning" : "error"}>
      {stale ? <RefreshCwIcon /> : <TriangleAlertIcon />}
      <AlertDescription>{message}</AlertDescription>
      {onRetry === undefined ? null : (
        <AlertAction>
          <Button onClick={onRetry} size="xs" variant="outline">
            Retry
          </Button>
        </AlertAction>
      )}
    </Alert>
  );
}

export function ActivityPanel({
  timestampFormat,
  route,
  snapshot,
  roster,
  detail,
  onNavigate,
  onLoadMoreRoster,
  onLoadMoreDetail,
  onRefreshSnapshot,
  onCancelActor,
  onRetryCancellation,
  cancellationError = null,
  now,
}: ActivityPanelProps) {
  const headingRef = useRef<HTMLHeadingElement>(null);
  const selectedForFocusRef = useRef<string | null>(null);
  const handledRemovalRef = useRef<string | null>(null);
  const previousSectionRef = useRef(route.section);
  const [restoreFocusRecordId, setRestoreFocusRecordId] = useState<string | null>(null);
  const [removalNotice, setRemovalNotice] = useState<{
    readonly section: ActivityPanelRoute["section"];
    readonly recordKind: ActivityRecordKind;
    readonly recordId: string;
  } | null>(null);
  const selected = route.selectedRecordKind !== null && route.selectedRecordId !== null;
  const routedDetail = detailMatchesRoute(detail, route) ? detail : null;
  const removed = selected && routedDetail?.removed === true;
  const detailRecord = routedDetail?.pages[0]?.record;
  const loadingDetail = routedDetail === null ? loadingDetailForRoute(route) : null;
  const resolvedDetail = routedDetail ?? loadingDetail;
  const activeRosterPages = roster.active.pages;
  const doneRosterPages = roster.done.pages;
  const reconciledRoster = useMemo(
    () =>
      reconcileActivityRosterRecords(
        { pages: activeRosterPages },
        { pages: doneRosterPages },
        route.section,
      ),
    [activeRosterPages, doneRosterPages, route.section],
  );
  const rosterRecords = useMemo(
    () => [...reconciledRoster.active, ...reconciledRoster.done],
    [reconciledRoster],
  );
  const sectionHealth = snapshot.sections[route.section];
  const effectiveNow = now ?? snapshot.updatedAt;
  const removalNotification =
    removed || removalNotice?.section === route.section
      ? "This activity record is no longer available."
      : null;
  const requestedCancellation = snapshot.control.operations.some(
    (operation) => operation.state === "requested",
  );
  const partialCancellations = snapshot.control.operations.filter(
    (operation) => operation.state === "partial",
  );

  useEffect(() => {
    if (previousSectionRef.current === route.section) {
      return;
    }
    previousSectionRef.current = route.section;
    handledRemovalRef.current = null;
    setRemovalNotice(null);
  }, [route.section]);

  // A handled removal always has its notice; selecting another record forgets both.
  useEffect(() => {
    const handledRemoval = handledRemovalRef.current;
    if (
      handledRemoval === null ||
      route.selectedRecordKind === null ||
      route.selectedRecordId === null ||
      handledRemoval === `${route.section}:${route.selectedRecordKind}:${route.selectedRecordId}`
    ) {
      return;
    }
    handledRemovalRef.current = null;
    setRemovalNotice(null);
  }, [route.section, route.selectedRecordId, route.selectedRecordKind]);

  const headingFocusKey =
    selected && !removed ? `${route.selectedRecordId}\u0000${detailRecord?.id ?? ""}` : null;
  useEffect(() => {
    if (headingFocusKey !== null) {
      headingRef.current?.focus();
    }
  }, [headingFocusKey]);

  useEffect(() => {
    if (!removed || route.selectedRecordKind === null || route.selectedRecordId === null) {
      return;
    }
    const removalKey = `${route.section}:${route.selectedRecordKind}:${route.selectedRecordId}`;
    if (handledRemovalRef.current === removalKey) {
      return;
    }
    handledRemovalRef.current = removalKey;
    setRestoreFocusRecordId(null);
    setRemovalNotice({
      section: route.section,
      recordKind: route.selectedRecordKind,
      recordId: route.selectedRecordId,
    });
    onNavigate({
      section: route.section,
      selectedRecordKind: null,
      selectedRecordId: null,
    });
  }, [onNavigate, removed, route.section, route.selectedRecordId, route.selectedRecordKind]);

  const selectRecord = useCallback(
    (record: ActivityRecordSummary) => {
      selectedForFocusRef.current = record.id;
      handledRemovalRef.current = null;
      setRemovalNotice(null);
      onNavigate({
        section: route.section,
        selectedRecordKind: record._tag,
        selectedRecordId: record.id,
      });
    },
    [onNavigate, route.section],
  );
  const back = useCallback(() => {
    setRestoreFocusRecordId(selectedForFocusRef.current ?? route.selectedRecordId);
    onNavigate({
      section: route.section,
      selectedRecordKind: null,
      selectedRecordId: null,
    });
  }, [onNavigate, route.section, route.selectedRecordId]);
  const selectActor = useCallback(
    (actor: Extract<ActivityRecordSummary, { readonly _tag: "actor" }>) => {
      selectedForFocusRef.current = actor.id;
      handledRemovalRef.current = null;
      setRemovalNotice(null);
      onNavigate({
        section: "subagents",
        selectedRecordKind: "actor",
        selectedRecordId: actor.id,
      });
    },
    [onNavigate],
  );
  const focusRestored = useCallback(() => {
    setRestoreFocusRecordId(null);
  }, []);

  const rosterContent = (
    <ActivityRoster
      active={roster.active}
      done={roster.done}
      focusRecordId={restoreFocusRecordId}
      notification={removalNotification}
      now={effectiveNow}
      onFocusRestored={focusRestored}
      onLoadMore={onLoadMoreRoster}
      {...(onCancelActor === undefined ? {} : { onCancelActor })}
      onSelect={selectRecord}
      reconciled={reconciledRoster}
      section={route.section}
      snapshot={snapshot}
    />
  );

  return (
    <div className="flex size-full min-h-0 flex-col bg-background" data-activity-panel>
      {snapshot.observationState === "stale" || snapshot.observationState === "reconnecting" ? (
        <RetryBanner message="Activity data is stale. Showing the last known activity." stale />
      ) : snapshot.observationState === "error" ? (
        <RetryBanner
          message="Activity updates failed. Showing the last known activity."
          onRetry={onRefreshSnapshot}
        />
      ) : null}
      {sectionHealth.state === "stale" || sectionHealth.state === "error" ? (
        <RetryBanner
          message={
            sectionHealth.message ??
            `${route.section === "subagents" ? "Subagents" : "Background tasks"} data is ${sectionHealth.state}.`
          }
          {...(sectionHealth.retryable ? { onRetry: onRefreshSnapshot } : {})}
          stale={sectionHealth.state === "stale"}
        />
      ) : null}
      {!requestedCancellation && cancellationError !== null ? (
        <RetryBanner message={cancellationError} />
      ) : null}
      {snapshot.scope._tag === "thread"
        ? partialCancellations.map((operation) => (
            <Alert className="mx-3 mt-3" key={operation.rootActorId} variant="error">
              <TriangleAlertIcon />
              <AlertDescription>
                {operation.message ?? "Some agents are still running."} {operation.residualCount}{" "}
                remaining.
              </AlertDescription>
              {onRetryCancellation === undefined ? null : (
                <AlertAction>
                  <Button
                    onClick={() =>
                      onRetryCancellation(operation.rootActorId, operation.operationRevision)
                    }
                    size="xs"
                    variant="outline"
                  >
                    Retry remaining
                  </Button>
                </AlertAction>
              )}
            </Alert>
          ))
        : null}
      <ScrollArea className="min-h-0 flex-1">
        {selected && !removed && resolvedDetail !== null ? (
          <ActivityRecordDetail
            headingRef={headingRef}
            onBack={back}
            onLoadMore={onLoadMoreDetail}
            onSelectActor={selectActor}
            query={resolvedDetail}
            rosterRecords={rosterRecords}
            section={route.section}
            snapshot={snapshot}
            timestampFormat={timestampFormat}
          />
        ) : (
          rosterContent
        )}
      </ScrollArea>
    </div>
  );
}
