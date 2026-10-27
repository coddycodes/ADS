import {
  ACTIVITY_PAGE_MAX_LENGTH,
  type ActivityLifecycle,
  type ActivitySection,
  type ActivitySnapshot,
} from "@coddy/contracts";
import {
  BotIcon,
  ChevronDownIcon,
  Clock3Icon,
  ListTodoIcon,
  RefreshCwIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { PROVIDER_ICON_BY_PROVIDER } from "~/components/chat/providerIconUtils";
import { Button } from "~/components/ui/button";
import { usePrefersReducedMotion } from "~/hooks/useMediaQuery";
import { cn } from "~/lib/utils";
import { ACTIVITY_DOCK_SHEET_INSET_CLASS_NAME } from "~/rightPanelLayout";
import { activityElapsedLabel, selectActivityDockVisibility } from "./activityPresentation";

const LIVE_ANNOUNCEMENT_WINDOW_MS = 500;

function safeCount(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    return 0;
  }
  return Math.min(Math.trunc(value), Number.MAX_SAFE_INTEGER);
}

function addCounts(left: number, right: number): number {
  const total = left + right;
  return Number.isSafeInteger(total) ? total : Number.MAX_SAFE_INTEGER;
}

function countLabel(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function resolveProviderIcon(provider: string) {
  if (!Object.hasOwn(PROVIDER_ICON_BY_PROVIDER, provider)) {
    return BotIcon;
  }
  const icon = PROVIDER_ICON_BY_PROVIDER[provider as ActivitySnapshot["provider"]];
  return typeof icon === "function" ? icon : BotIcon;
}

function renderProviderIcon(provider: string, className: string) {
  const ProviderIcon = resolveProviderIcon(provider);
  return <ProviderIcon className={className} />;
}

function isActiveStatus(status: ActivityLifecycle): boolean {
  return (
    status === "starting" || status === "running" || status === "waiting" || status === "unknown"
  );
}

interface ActivityDockRecord {
  readonly id: string;
  readonly name: string;
  readonly status: ActivityLifecycle;
  readonly startedAt: string;
}

interface ActivityDockViewModel {
  readonly provider: string;
  readonly observationState: ActivitySnapshot["observationState"];
  readonly sections: ActivitySnapshot["sections"];
  readonly counts: ActivitySnapshot["counts"];
  readonly actors: ReadonlyArray<ActivityDockRecord>;
  readonly workItems: ReadonlyArray<ActivityDockRecord>;
  readonly updatedAt: string;
  readonly visibility: ReturnType<typeof selectActivityDockVisibility>;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isActivityLifecycle(value: unknown): value is ActivityLifecycle {
  return (
    value === "starting" ||
    value === "running" ||
    value === "waiting" ||
    value === "completed" ||
    value === "failed" ||
    value === "cancelled" ||
    value === "interrupted" ||
    value === "unknown"
  );
}

function normalizeSectionHealth(value: unknown): ActivitySnapshot["sections"]["subagents"] {
  const state =
    isObject(value) &&
    (value.state === "live" ||
      value.state === "stale" ||
      value.state === "error" ||
      value.state === "unsupported")
      ? value.state
      : "unsupported";
  return { state, message: null, retryable: false };
}

function normalizeSectionCounts(value: unknown): ActivitySnapshot["counts"]["subagents"] {
  return {
    active: safeCount(isObject(value) ? value.active : undefined),
    done: safeCount(isObject(value) ? value.done : undefined),
  };
}

function normalizeRecord(value: unknown): ActivityDockRecord | null {
  if (
    !isObject(value) ||
    typeof value.id !== "string" ||
    value.id.length === 0 ||
    typeof value.name !== "string" ||
    value.name.length === 0 ||
    !isActivityLifecycle(value.status) ||
    typeof value.startedAt !== "string"
  ) {
    return null;
  }
  return {
    id: value.id,
    name: value.name,
    status: value.status,
    startedAt: value.startedAt,
  };
}

function normalizeRecords(values: ReadonlyArray<unknown>): ReadonlyArray<ActivityDockRecord> {
  const records: ActivityDockRecord[] = [];
  const length = Math.min(values.length, ACTIVITY_PAGE_MAX_LENGTH);
  for (let index = 0; index < length; index += 1) {
    const record = normalizeRecord(values[index]);
    if (record !== null) {
      records.push(record);
    }
  }
  return records;
}

function normalizeActivityDockSnapshot(snapshot: unknown): ActivityDockViewModel | null {
  if (!isObject(snapshot)) {
    return null;
  }
  const capabilities = snapshot.capabilities;
  const sections = snapshot.sections;
  const counts = snapshot.counts;
  const actors = snapshot.actors;
  const workItems = snapshot.workItems;
  if (
    !isObject(capabilities) ||
    !isObject(sections) ||
    !isObject(counts) ||
    !Array.isArray(actors) ||
    !Array.isArray(workItems)
  ) {
    return null;
  }

  const normalizedSections = {
    subagents: normalizeSectionHealth(sections.subagents),
    backgroundTasks: normalizeSectionHealth(sections.backgroundTasks),
  };
  const normalizedCounts = {
    subagents: normalizeSectionCounts(counts.subagents),
    backgroundTasks: normalizeSectionCounts(counts.backgroundTasks),
  };
  const visibilitySnapshot = {
    capabilities: {
      actors: capabilities.actors === true,
      backgroundWork: capabilities.backgroundWork === true,
    },
    sections: normalizedSections,
    counts: normalizedCounts,
  } as unknown as ActivitySnapshot;
  const visibility = selectActivityDockVisibility(visibilitySnapshot);

  return {
    provider:
      typeof snapshot.provider === "string" && snapshot.provider.length > 0
        ? snapshot.provider
        : "unknown",
    observationState:
      snapshot.observationState === "reconnecting" ||
      snapshot.observationState === "stale" ||
      snapshot.observationState === "error"
        ? snapshot.observationState
        : "live",
    sections: normalizedSections,
    counts: normalizedCounts,
    actors: visibility.showSubagents ? normalizeRecords(actors) : [],
    workItems: visibility.showBackgroundTasks ? normalizeRecords(workItems) : [],
    updatedAt: typeof snapshot.updatedAt === "string" ? snapshot.updatedAt : "",
    visibility,
  };
}

type DegradedSectionState = "stale" | "error";

function degradedSectionState(state: string): DegradedSectionState | null {
  return state === "stale" || state === "error" ? state : null;
}

function SectionStatus({ state }: { readonly state: DegradedSectionState | null }) {
  if (state === null) {
    return null;
  }
  const Icon = state === "error" ? TriangleAlertIcon : RefreshCwIcon;
  return (
    <span
      aria-hidden="true"
      className="flex size-4 shrink-0 items-center justify-center text-muted-foreground"
      data-activity-section-status={state}
      title={state === "error" ? "Error" : "Stale"}
    >
      <Icon className="size-3.5" />
    </span>
  );
}

function firstActiveRecord(
  records: ReadonlyArray<ActivityDockRecord>,
): ActivityDockRecord | undefined {
  return records
    .filter((record) => isActiveStatus(record.status))
    .sort(
      (left, right) =>
        compareText(left.startedAt, right.startedAt) || compareText(left.id, right.id),
    )[0];
}

function ActivityLiveAnnouncement({ announcement }: { readonly announcement: string }) {
  const [published, setPublished] = useState(announcement);
  const initialAnnouncement = useRef(true);

  useEffect(() => {
    if (initialAnnouncement.current) {
      initialAnnouncement.current = false;
      return;
    }
    const timer = window.setTimeout(() => setPublished(announcement), LIVE_ANNOUNCEMENT_WINDOW_MS);
    return () => window.clearTimeout(timer);
  }, [announcement]);

  return (
    <span aria-atomic="true" aria-live="polite" className="sr-only" role="status">
      {published}
    </span>
  );
}

export interface ActivityDockProps {
  readonly snapshot: ActivitySnapshot;
  readonly expanded: boolean;
  readonly compact: boolean;
  readonly avoidRightPanelSheet?: boolean;
  readonly onExpandedChange: (expanded: boolean) => void;
  readonly onOpenSection: (section: ActivitySection) => void;
  readonly now?: string;
}

export function ActivityDock({
  snapshot,
  expanded,
  compact,
  avoidRightPanelSheet = false,
  onExpandedChange,
  onOpenSection,
  now,
}: ActivityDockProps) {
  const prefersReducedMotion = usePrefersReducedMotion();
  const viewModel = normalizeActivityDockSnapshot(snapshot);
  if (viewModel === null || !viewModel.visibility.visible) {
    return null;
  }
  const visibility = viewModel.visibility;

  const subagentActive = visibility.showSubagents ? viewModel.counts.subagents.active : 0;
  const subagentDone = visibility.showSubagents ? viewModel.counts.subagents.done : 0;
  const backgroundActive = visibility.showBackgroundTasks
    ? viewModel.counts.backgroundTasks.active
    : 0;
  const backgroundDone = visibility.showBackgroundTasks ? viewModel.counts.backgroundTasks.done : 0;
  const active = addCounts(subagentActive, backgroundActive);
  const done = addCounts(subagentDone, backgroundDone);
  const dataIsStale =
    viewModel.observationState === "reconnecting" || viewModel.observationState === "stale";
  const subagentSectionState = visibility.showSubagents
    ? degradedSectionState(viewModel.sections.subagents.state)
    : null;
  const backgroundSectionState = visibility.showBackgroundTasks
    ? degradedSectionState(viewModel.sections.backgroundTasks.state)
    : null;
  const openSection = (section: ActivitySection) => {
    onExpandedChange(false);
    onOpenSection(section);
  };
  const accessibleCounts = [
    ...(visibility.showSubagents
      ? [countLabel(subagentActive, "active subagent"), countLabel(subagentDone, "done subagent")]
      : []),
    ...(visibility.showBackgroundTasks
      ? [
          countLabel(backgroundActive, "active background task"),
          countLabel(backgroundDone, "done background task"),
        ]
      : []),
  ].join(", ");
  const liveAnnouncement = `Activity update: ${accessibleCounts}${
    dataIsStale ? ". Activity data stale" : ""
  }${subagentSectionState === null ? "" : `. Subagents ${subagentSectionState}`}${
    backgroundSectionState === null ? "" : `. Background tasks ${backgroundSectionState}`
  }`;
  const elapsedNow = now ?? viewModel.updatedAt;
  const activeSubagent = firstActiveRecord(viewModel.actors);
  const activeBackgroundTask = firstActiveRecord(viewModel.workItems);
  const providerGlyph = (
    <span
      aria-hidden="true"
      className="flex size-5 shrink-0 items-center justify-center rounded-full border border-border bg-muted"
      data-activity-provider-glyph={viewModel.provider}
    >
      {renderProviderIcon(viewModel.provider, "size-3")}
    </span>
  );

  const toggleContent = expanded ? (
    <>
      {providerGlyph}
      <span className="truncate">Activity</span>
      <ChevronDownIcon aria-hidden="true" className="ml-auto size-4" />
    </>
  ) : (
    <>
      {providerGlyph}
      {compact ? (
        <>
          <span
            aria-hidden="true"
            className="whitespace-nowrap text-xs tabular-nums"
            data-activity-count="active"
          >
            Active {active}
          </span>
          <span
            aria-hidden="true"
            className="text-base leading-none font-bold text-foreground"
            data-activity-count-separator="true"
          >
            ·
          </span>
          <span
            aria-hidden="true"
            className="whitespace-nowrap text-xs text-muted-foreground tabular-nums"
            data-activity-count="done"
          >
            Done {done}
          </span>
        </>
      ) : (
        <>
          <span className="whitespace-nowrap text-xs tabular-nums">Active {active}</span>
          <span className="whitespace-nowrap text-xs text-muted-foreground tabular-nums">
            Done {done}
          </span>
        </>
      )}
    </>
  );

  return (
    <div
      className={cn(
        "pointer-events-none absolute top-3 z-20",
        avoidRightPanelSheet ? ACTIVITY_DOCK_SHEET_INSET_CLASS_NAME : "right-3",
      )}
      data-testid="activity-dock"
    >
      <div
        aria-label={dataIsStale ? "Activity data stale" : undefined}
        className={cn(
          "pointer-events-auto max-w-72 rounded-lg border border-border bg-popover text-popover-foreground shadow-md",
          !prefersReducedMotion && "transition-[width,opacity] duration-150",
          expanded ? "w-72" : "w-fit",
        )}
        data-activity-motion={prefersReducedMotion ? "reduced" : "normal"}
        onKeyDown={(event) => {
          if (expanded && event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onExpandedChange(false);
          }
        }}
        role="group"
      >
        <Button
          aria-expanded={expanded}
          aria-label={`${expanded ? "Collapse" : "Expand"} activity summary: ${accessibleCounts}`}
          className="h-9 min-h-9 w-full min-w-9 gap-2 px-2"
          onClick={() => onExpandedChange(!expanded)}
          size="sm"
          variant="ghost"
        >
          {toggleContent}
        </Button>
        {expanded ? (
          <div className="border-t border-panel-separator p-1">
            {visibility.showSubagents ? (
              <Button
                aria-label={`Open Subagents: ${subagentActive} active, ${subagentDone} done${
                  subagentSectionState === null ? "" : `. Status: ${subagentSectionState}`
                }`}
                className="w-full justify-start px-2 py-1.5 text-left"
                data-activity-section="subagents"
                onClick={() => openSection("subagents")}
                size="content"
                variant="ghost"
              >
                <BotIcon aria-hidden="true" className="size-4" />
                <SectionStatus state={subagentSectionState} />
                <span
                  className="flex min-w-0 flex-1 flex-col"
                  data-activity-section-copy="subagents"
                >
                  <span
                    className="flex min-w-0 items-center gap-2"
                    data-activity-section-primary="subagents"
                  >
                    <span className="min-w-0 flex-1 truncate">Subagents</span>
                    <span className="shrink-0 whitespace-nowrap text-xs tabular-nums">
                      Active {subagentActive}
                    </span>
                    <span className="shrink-0 whitespace-nowrap text-xs text-muted-foreground tabular-nums">
                      Done {subagentDone}
                    </span>
                  </span>
                  {activeSubagent === undefined ? null : (
                    <span
                      aria-hidden="true"
                      className="mt-0.5 flex items-center text-xs text-muted-foreground tabular-nums"
                      data-activity-section-metadata="subagents"
                    >
                      <Clock3Icon className="mr-1 size-3 shrink-0" />
                      <span className="truncate">
                        {activityElapsedLabel(activeSubagent.startedAt, elapsedNow)}
                      </span>
                    </span>
                  )}
                </span>
              </Button>
            ) : null}
            {visibility.showBackgroundTasks ? (
              <Button
                aria-label={`Open Background tasks: ${backgroundActive} active, ${backgroundDone} done${
                  backgroundSectionState === null ? "" : `. Status: ${backgroundSectionState}`
                }`}
                className="w-full justify-start px-2 py-1.5 text-left"
                data-activity-section="backgroundTasks"
                onClick={() => openSection("backgroundTasks")}
                size="content"
                variant="ghost"
              >
                <ListTodoIcon aria-hidden="true" className="size-4" />
                <SectionStatus state={backgroundSectionState} />
                <span
                  className="flex min-w-0 flex-1 flex-col"
                  data-activity-section-copy="backgroundTasks"
                >
                  <span
                    className="flex min-w-0 items-center gap-2"
                    data-activity-section-primary="backgroundTasks"
                  >
                    <span className="min-w-0 flex-1 truncate">Background tasks</span>
                    <span className="shrink-0 whitespace-nowrap text-xs tabular-nums">
                      Active {backgroundActive}
                    </span>
                    <span className="shrink-0 whitespace-nowrap text-xs text-muted-foreground tabular-nums">
                      Done {backgroundDone}
                    </span>
                  </span>
                  {activeBackgroundTask === undefined ? null : (
                    <span
                      aria-hidden="true"
                      className="mt-0.5 flex items-center text-xs text-muted-foreground tabular-nums"
                      data-activity-section-metadata="backgroundTasks"
                    >
                      <Clock3Icon className="mr-1 size-3 shrink-0" />
                      <span className="truncate">
                        {activityElapsedLabel(activeBackgroundTask.startedAt, elapsedNow)}
                      </span>
                    </span>
                  )}
                </span>
              </Button>
            ) : null}
          </div>
        ) : null}
        <ActivityLiveAnnouncement announcement={liveAnnouncement} />
      </div>
    </div>
  );
}
