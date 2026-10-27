import type { ActivityEntry } from "@coddy/contracts";
import type { TimestampFormat } from "@coddy/contracts/settings";
import {
  ActivityIcon,
  CheckCircle2Icon,
  CircleAlertIcon,
  FileCheck2Icon,
  MessageSquareTextIcon,
  TerminalSquareIcon,
  WrenchIcon,
} from "lucide-react";
import type { ComponentType } from "react";

import { cn } from "~/lib/utils";
import { formatChatTimestampTooltip } from "~/timestampFormat";

const COLLAPSIBLE_DETAIL_LENGTH = 4_096;

const ENTRY_PRESENTATION: Record<
  ActivityEntry["kind"],
  { readonly label: string; readonly icon: ComponentType<{ className?: string }> }
> = {
  commentary: { label: "Commentary", icon: MessageSquareTextIcon },
  tool: { label: "Tool", icon: WrenchIcon },
  command: { label: "Command", icon: TerminalSquareIcon },
  result: { label: "Result", icon: FileCheck2Icon },
  error: { label: "Error", icon: CircleAlertIcon },
  state: { label: "State", icon: ActivityIcon },
  completion: { label: "Completion", icon: CheckCircle2Icon },
};

export interface ActivityEntryRowProps {
  readonly entry: ActivityEntry;
  readonly timestampFormat: TimestampFormat;
}

function formatActivityEntryTimestamp(value: string, timestampFormat: TimestampFormat): string {
  return formatChatTimestampTooltip(value, timestampFormat) || value;
}

function EntryDetail({ entry }: Pick<ActivityEntryRowProps, "entry">) {
  if (entry.detail === null) {
    return null;
  }

  const content = (
    <pre className="mt-1 overflow-x-auto whitespace-pre-wrap break-words font-sans text-xs text-muted-foreground">
      {entry.detail}
    </pre>
  );
  if (entry.detail.length < COLLAPSIBLE_DETAIL_LENGTH) {
    return content;
  }

  return (
    <details className="mt-1" data-activity-entry-detail={entry.id}>
      <summary className="cursor-pointer text-xs text-muted-foreground">Show details</summary>
      {content}
    </details>
  );
}

export function ActivityEntryRow({ entry, timestampFormat }: ActivityEntryRowProps) {
  const presentation = ENTRY_PRESENTATION[entry.kind];
  const Icon = presentation.icon;

  return (
    <article
      className={cn(
        "rounded-lg border border-border/60 bg-card/30 px-3 py-2",
        entry.tone === "error" && "border-destructive/30 bg-destructive/5",
      )}
      data-activity-entry-id={entry.id}
      data-activity-entry-kind={entry.kind}
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <Icon aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
        <span
          className="shrink-0 text-xs font-medium text-muted-foreground"
          data-activity-entry-label
        >
          {presentation.label}
        </span>
        <span className="min-w-16 flex-1 truncate text-sm">{entry.title}</span>
        <time
          className="ml-auto shrink-0 text-xs text-muted-foreground"
          dateTime={entry.createdAt}
          title={entry.createdAt}
        >
          {formatActivityEntryTimestamp(entry.createdAt, timestampFormat)}
        </time>
      </div>
      <EntryDetail entry={entry} />
    </article>
  );
}
