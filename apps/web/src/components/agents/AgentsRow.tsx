import { ArrowUpRightIcon } from "lucide-react";
import { memo, useCallback } from "react";

import { cn } from "../../lib/utils";
import { selectIsUnread, useSidebarWorkspaceMetaStore } from "../../sidebarWorkspaceMetaStore";
import { formatRelativeTimeLabel } from "../../timestampFormat";
import { ProviderInstanceIcon } from "../chat/ProviderInstanceIcon";
import { SidebarMenuAction, SidebarMenuButton, SidebarMenuItem } from "../ui/sidebar";
import type { AgentRow } from "../sidebar/agentsSection.logic";

export interface AgentsRowProps {
  readonly row: AgentRow;
  readonly selected: boolean;
  readonly onSelect: (key: string) => void;
  readonly onJumpToWorkspace: (row: AgentRow) => void;
}

function samePill(left: AgentRow["pill"], right: AgentRow["pill"]): boolean {
  return (
    left === right ||
    (left?.label === right?.label &&
      left?.colorClass === right?.colorClass &&
      left?.dotClass === right?.dotClass &&
      left?.pulse === right?.pulse)
  );
}

function sameAgentRow(left: AgentRow, right: AgentRow): boolean {
  return (
    left === right ||
    (left.key === right.key &&
      left.shell === right.shell &&
      left.branch === right.branch &&
      left.environmentLabel === right.environmentLabel &&
      left.environmentLive === right.environmentLive &&
      left.environmentStatus === right.environmentStatus &&
      left.projectTitle === right.projectTitle &&
      left.providerDriverKind === right.providerDriverKind &&
      left.providerLabel === right.providerLabel &&
      left.previewLine === right.previewLine &&
      samePill(left.pill, right.pill))
  );
}

export const AgentsRow = memo(
  function AgentsRow({ row, selected, onSelect, onJumpToWorkspace }: AgentsRowProps) {
    const isUnread = useSidebarWorkspaceMetaStore((state) =>
      selectIsUnread(state.unreadThreadKeys, row.key),
    );
    const statusLabel = row.pill?.label ?? "Done";
    const environmentStatus = row.environmentLive ? null : row.environmentStatus;
    const eyebrow = row.projectTitle || row.environmentLabel;
    // Orca's card hierarchy: the branch is the line the eye lands on, the thread
    // title explains it, and the preview shows what the agent is doing now.
    const primaryLine = row.branch ?? row.shell.title;
    const secondaryLine = row.branch === null ? null : row.shell.title;
    const live = row.environmentLive;

    const handleSelect = useCallback(() => {
      onSelect(row.key);
    }, [onSelect, row.key]);
    const handleJumpToWorkspace = useCallback(() => {
      onJumpToWorkspace(row);
    }, [onJumpToWorkspace, row]);

    return (
      <SidebarMenuItem className="border-b border-border/60 last:border-b-0">
        <SidebarMenuButton
          size="lg"
          isActive={selected}
          className="h-auto items-stretch gap-0 rounded-md px-3 py-2.5 pr-9 hover:bg-accent/60 data-[active=true]:bg-accent data-[active=true]:font-normal"
          aria-label={`${row.shell.title}, ${statusLabel}, ${row.environmentLabel}`}
          aria-pressed={selected}
          onClick={handleSelect}
        >
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
              <span
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  row.pill?.dotClass ?? "bg-muted-foreground/40",
                  row.pill?.pulse && "animate-pulse",
                )}
                aria-hidden
              />
              {row.providerDriverKind !== null ? (
                <ProviderInstanceIcon
                  driverKind={row.providerDriverKind}
                  displayName={row.providerLabel ?? ""}
                  className="size-3.5"
                  iconClassName="size-3.5"
                  showBadge={false}
                />
              ) : null}
              <span className="min-w-0 truncate font-medium uppercase tracking-wide">
                {eyebrow}
              </span>
              <span className="ml-auto flex shrink-0 items-center gap-1.5 tabular-nums">
                {isUnread ? (
                  <span
                    aria-label="Unread"
                    className="size-1.5 shrink-0 rounded-full bg-sky-500 dark:bg-sky-300/80"
                  />
                ) : null}
                {formatRelativeTimeLabel(row.shell.updatedAt)}
              </span>
            </div>
            <div
              className={cn(
                "truncate text-sm font-medium",
                live ? "text-foreground" : "text-muted-foreground",
                isUnread && secondaryLine === null && "font-semibold",
              )}
            >
              {primaryLine}
            </div>
            {secondaryLine !== null ? (
              <div
                className={cn(
                  "truncate text-[13px]",
                  isUnread
                    ? "font-semibold text-foreground"
                    : live
                      ? "text-foreground/80"
                      : "text-muted-foreground",
                )}
              >
                {secondaryLine}
              </div>
            ) : null}
            {row.previewLine ? (
              <div className="truncate text-xs text-muted-foreground">{row.previewLine}</div>
            ) : null}
            <div className="flex min-w-0 items-center gap-1.5 pt-0.5 text-xs text-muted-foreground">
              <span
                className={cn(
                  "shrink-0 font-medium",
                  row.pill?.colorClass ?? "text-muted-foreground",
                )}
              >
                {statusLabel}
              </span>
              {row.providerLabel !== null ? (
                <>
                  <span aria-hidden>·</span>
                  <span className="truncate">{row.providerLabel}</span>
                </>
              ) : null}
              <span className="ml-auto shrink-0 rounded border border-border px-1.5 py-px font-medium">
                {row.environmentLabel}
                {environmentStatus ? ` · ${environmentStatus}` : null}
              </span>
            </div>
          </div>
        </SidebarMenuButton>
        <SidebarMenuAction
          showOnHover
          className="top-2 right-2 size-6 bg-background/80"
          aria-label={`Jump to workspace for ${row.shell.title}`}
          title="Jump to workspace"
          onClick={handleJumpToWorkspace}
        >
          <ArrowUpRightIcon className="size-3.5" aria-hidden />
        </SidebarMenuAction>
      </SidebarMenuItem>
    );
  },
  (previous, next) =>
    previous.selected === next.selected &&
    previous.onSelect === next.onSelect &&
    previous.onJumpToWorkspace === next.onJumpToWorkspace &&
    sameAgentRow(previous.row, next.row),
);
