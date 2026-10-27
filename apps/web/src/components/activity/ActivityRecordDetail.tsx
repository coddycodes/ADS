import {
  ACTIVITY_PAGE_MAX_LENGTH,
  type ActivityActorSummary,
  type ActivityEntry,
  type ActivityRecordId,
  type ActivityRecordKind,
  type ActivityRecordSummary,
  type ActivitySection,
  type ActivitySnapshot,
} from "@coddy/contracts";
import type { TimestampFormat } from "@coddy/contracts/settings";
import { ArrowLeftIcon } from "lucide-react";
import { useMemo, type RefObject } from "react";

import { Alert, AlertAction, AlertDescription } from "~/components/ui/alert";
import { Button } from "~/components/ui/button";
import { Spinner } from "~/components/ui/spinner";
import { activityStatusLabel, compareActivityTimestamps } from "./activityPresentation";
import { ActivityEntryRow } from "./ActivityEntryRow";
import type { ActivityDetailPageData, ActivityDetailQueryResult } from "./ActivityPanel";
import { formatChatTimestampTooltip } from "~/timestampFormat";

const WINDOW_GROUP_SIZE = 50;

function formatActivityTimestamp(value: string, timestampFormat: TimestampFormat): string {
  return formatChatTimestampTooltip(value, timestampFormat) || value;
}

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function entriesFromPages(
  pages: ReadonlyArray<ActivityDetailPageData>,
  recordKind: ActivityRecordKind,
  recordId: ActivityRecordId,
): ActivityEntry[] {
  const seen = new Set<string>();
  const entries: ActivityEntry[] = [];
  for (const page of pages) {
    for (const entry of page.entries) {
      if (entry.ownerKind !== recordKind || entry.ownerId !== recordId || seen.has(entry.id)) {
        continue;
      }
      seen.add(entry.id);
      entries.push(entry);
      if (entries.length >= ACTIVITY_PAGE_MAX_LENGTH) {
        break;
      }
    }
    if (entries.length >= ACTIVITY_PAGE_MAX_LENGTH) {
      break;
    }
  }
  return entries.sort(
    (left, right) =>
      compareActivityTimestamps(left.createdAt, right.createdAt) || compareText(left.id, right.id),
  );
}

function entryGroups(
  entries: ReadonlyArray<ActivityEntry>,
): ReadonlyArray<ReadonlyArray<ActivityEntry>> {
  const groups: ActivityEntry[][] = [];
  for (let index = 0; index < entries.length; index += WINDOW_GROUP_SIZE) {
    groups.push(entries.slice(index, index + WINDOW_GROUP_SIZE));
  }
  return groups;
}

function recordTypeLabel(record: ActivityRecordSummary): string {
  return record._tag === "actor" ? "Actor" : `Background task · ${record.workKind}`;
}

function scopeActors(
  snapshot: ActivitySnapshot,
  rosterRecords: ReadonlyArray<ActivityRecordSummary>,
): ReadonlyArray<ActivityActorSummary> {
  const byId = new Map<string, ActivityActorSummary>();
  for (const actor of snapshot.actors) {
    byId.set(actor.id, actor);
  }
  for (const record of rosterRecords) {
    if (record._tag === "actor") {
      byId.set(record.id, record);
    }
  }
  return [...byId.values()];
}

function relation(
  label: string,
  relationKind: "parent" | "owner",
  actorId: string,
  actors: ReadonlyArray<ActivityActorSummary>,
  onSelectActor: (actor: ActivityActorSummary) => void,
) {
  const actor = actors.find((candidate) => candidate.id === actorId);
  return (
    <div className="flex gap-2 text-xs">
      <dt className="text-muted-foreground">{label}</dt>
      <dd>
        {actor === undefined ? (
          <span>{actorId}</span>
        ) : (
          <Button
            className="h-auto p-0 text-xs"
            data-activity-relation={relationKind}
            onClick={() => onSelectActor(actor)}
            variant="link"
          >
            {actor.name}
          </Button>
        )}
      </dd>
    </div>
  );
}

export interface ActivityRecordDetailProps {
  readonly timestampFormat: TimestampFormat;
  readonly section: ActivitySection;
  readonly snapshot: ActivitySnapshot;
  readonly query: ActivityDetailQueryResult;
  readonly rosterRecords: ReadonlyArray<ActivityRecordSummary>;
  readonly headingRef: RefObject<HTMLHeadingElement | null>;
  readonly onBack: () => void;
  readonly onSelectActor: (actor: ActivityActorSummary) => void;
  readonly onLoadMore: () => void;
}

export function ActivityRecordDetail({
  timestampFormat,
  section,
  snapshot,
  query,
  rosterRecords,
  headingRef,
  onBack,
  onSelectActor,
  onLoadMore,
}: ActivityRecordDetailProps) {
  const record =
    rosterRecords.find(
      (candidate) => candidate._tag === query.recordKind && candidate.id === query.recordId,
    ) ??
    query.pages[0]?.record ??
    null;
  const entries = useMemo(
    () => entriesFromPages(query.pages, query.recordKind, query.recordId),
    [query.pages, query.recordId, query.recordKind],
  );
  const entryWindows = useMemo(() => entryGroups(entries), [entries]);
  const actors = useMemo(() => scopeActors(snapshot, rosterRecords), [rosterRecords, snapshot]);
  const nextCursor = query.pages.at(-1)?.nextCursor ?? null;
  const sectionName = section === "subagents" ? "Subagents" : "Background Tasks";

  if (record === null) {
    return (
      <section className="flex min-h-0 flex-1 flex-col">
        <header className="border-b border-border/60 px-3 py-2">
          <Button aria-label={`Back to ${sectionName}`} onClick={onBack} size="sm" variant="ghost">
            <ArrowLeftIcon aria-hidden="true" />
            {sectionName}
          </Button>
        </header>
        {query.error === null ? (
          <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
            <Spinner className="size-4" />
            Loading record…
          </div>
        ) : (
          <Alert className="m-3" variant="error">
            <AlertDescription>{query.error}</AlertDescription>
            <AlertAction>
              <Button onClick={onLoadMore} size="xs" variant="outline">
                Retry
              </Button>
            </AlertAction>
          </Alert>
        )}
      </section>
    );
  }

  return (
    <section className="flex min-h-0 flex-1 flex-col">
      <header className="space-y-3 border-b border-border/60 px-3 py-2">
        <Button aria-label={`Back to ${sectionName}`} onClick={onBack} size="sm" variant="ghost">
          <ArrowLeftIcon aria-hidden="true" />
          {sectionName}
        </Button>
        <div>
          <h2
            className="text-base font-semibold outline-none"
            data-activity-detail-heading
            ref={headingRef}
            tabIndex={-1}
          >
            {record.name}
          </h2>
          <dl className="mt-2 grid gap-1">
            <div className="flex gap-2 text-xs">
              <dt className="text-muted-foreground">Type</dt>
              <dd>{recordTypeLabel(record)}</dd>
            </div>
            <div className="flex gap-2 text-xs">
              <dt className="text-muted-foreground">Lifecycle</dt>
              <dd>{activityStatusLabel(record.status)}</dd>
            </div>
            <div className="flex gap-2 text-xs">
              <dt className="text-muted-foreground">Provider</dt>
              <dd>
                {record._tag === "actor"
                  ? (record.providerType ?? snapshot.provider)
                  : snapshot.provider}
              </dd>
            </div>
            <div className="flex gap-2 text-xs">
              <dt className="text-muted-foreground">Started</dt>
              <dd>
                <time dateTime={record.startedAt} title={record.startedAt}>
                  {formatActivityTimestamp(record.startedAt, timestampFormat)}
                </time>
              </dd>
            </div>
            <div className="flex gap-2 text-xs">
              <dt className="text-muted-foreground">Ended</dt>
              <dd>
                {record.terminalAt === null ? (
                  "—"
                ) : (
                  <time dateTime={record.terminalAt} title={record.terminalAt}>
                    {formatActivityTimestamp(record.terminalAt, timestampFormat)}
                  </time>
                )}
              </dd>
            </div>
            {record._tag === "actor" && record.parentActorId !== null
              ? relation("Parent actor", "parent", record.parentActorId, actors, onSelectActor)
              : null}
            {record._tag === "workItem" && record.ownerActorId !== null
              ? relation("Owner actor", "owner", record.ownerActorId, actors, onSelectActor)
              : null}
          </dl>
        </div>
      </header>
      <div className="space-y-2 p-3">
        {query.error !== null ? (
          <Alert variant="error">
            <AlertDescription>
              {query.error} The last loaded entries remain available.
            </AlertDescription>
            <AlertAction>
              <Button onClick={onLoadMore} size="xs" variant="outline">
                Retry
              </Button>
            </AlertAction>
          </Alert>
        ) : null}
        {entries.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {query.loading ? "Loading record entries…" : "No activity entries recorded."}
          </p>
        ) : (
          entryWindows.map((window, windowIndex) => (
            <div
              className="space-y-2"
              data-activity-entry-window-group={windowIndex}
              key={window[0]?.id}
            >
              {window.map((entry) => (
                <ActivityEntryRow entry={entry} key={entry.id} timestampFormat={timestampFormat} />
              ))}
            </div>
          ))
        )}
        {nextCursor !== null ? (
          <Button
            className="w-full"
            disabled={query.loading}
            onClick={onLoadMore}
            size="sm"
            variant="outline"
          >
            {query.loading ? "Loading more…" : "Load more entries"}
          </Button>
        ) : null}
      </div>
    </section>
  );
}
