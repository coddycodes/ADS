import { useNavigate } from "@tanstack/react-router";
import { EllipsisIcon } from "lucide-react";
import * as React from "react";
import { useMemo } from "react";

import type { EnvironmentId } from "@coddy/contracts";

import { environmentCatalog } from "../../connection/catalog";
import { cn } from "../../lib/utils";
import { useActiveEnvironmentId } from "../../state/entities";
import { useEnvironment } from "../../state/environments";
import { useAtomCommand } from "../../state/use-atom-command";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { buildEnvironmentContextCardView } from "./environmentContextCard.logic";
import type { EnvironmentRailStatus } from "./environmentRail.logic";

const CARD_STATUS_DOT_CLASS: Record<EnvironmentRailStatus, string> = {
  connected: "bg-success",
  disconnected: "bg-muted-foreground/50",
  attention: "bg-warning",
  error: "bg-destructive",
};

export interface EnvironmentContextCardProps {
  readonly updateBadge?: React.ReactNode;
  readonly onCheckForUpdates?: (environmentId: EnvironmentId) => void;
  readonly onShowUpdateSteps?: (environmentId: EnvironmentId) => void;
  readonly updateInProgress?: boolean;
}

/** Remote-environment context and actions; hidden for this machine. */
export function EnvironmentContextCard(props: EnvironmentContextCardProps) {
  const activeEnvironmentId = useActiveEnvironmentId();
  const environment = useEnvironment(activeEnvironmentId);
  const navigate = useNavigate();
  const disconnectEnvironment = useAtomCommand(environmentCatalog.disconnect, {
    reportFailure: false,
  });

  const view = useMemo(
    () =>
      environment === null
        ? null
        : buildEnvironmentContextCardView({
            label: environment.label,
            target: environment.entry.target,
            connection: environment.connection,
            serverConfig: environment.serverConfig,
          }),
    [environment],
  );

  if (view === null || activeEnvironmentId === null) {
    return null;
  }

  // A failure reason ("No data from … for 30 seconds. …") runs to several
  // lines, so it takes its own line, clamped to three, and the version moves
  // to the badge row. The clamp is visual only: assistive technology reads the
  // whole reason, the tooltip shows it on hover (the help cursor hints at it),
  // and Tab reaches the line to open the tooltip from the keyboard.
  const reasonLine = view.hasReason ? (
    <Tooltip>
      <TooltipTrigger
        render={
          <div
            data-testid="environment-context-card-status"
            tabIndex={0}
            className="line-clamp-3 wrap-break-word cursor-help rounded-sm text-xs text-muted-foreground outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
          />
        }
      >
        {view.statusText}
      </TooltipTrigger>
      <TooltipPopup side="top" className="max-w-80 whitespace-normal leading-tight">
        {view.statusText}
      </TooltipPopup>
    </Tooltip>
  ) : null;

  return (
    <div
      data-testid="environment-context-card"
      className="mx-2 mb-1 flex items-center gap-2 rounded-[10px] border border-border bg-background px-2.5 py-2"
    >
      <span
        data-status={view.status}
        className={cn("size-2 shrink-0 rounded-full", CARD_STATUS_DOT_CLASS[view.status])}
      />
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-semibold text-foreground">{view.name}</div>
        {reasonLine}
        {/* Badges wrap onto their own row in a narrow sidebar instead of being clipped. */}
        <div className="flex min-w-0 flex-wrap items-center gap-x-1 gap-y-0.5 text-xs text-muted-foreground">
          {view.hasReason ? (
            view.versionLine ? (
              <span className="shrink-0">{view.versionLine}</span>
            ) : null
          ) : (
            // A short status wraps with its version instead of shrinking to "Co…"
            // at the narrowest sidebar; the no-break space keeps "·" off a line start.
            <span
              data-testid="environment-context-card-status"
              className="min-w-0 line-clamp-3 wrap-break-word"
            >
              {view.statusText}
              {view.versionLine ? (
                <>
                  {"\u00a0"}
                  <span aria-hidden>·</span>{" "}
                  <span className="whitespace-nowrap">{view.versionLine}</span>
                </>
              ) : null}
            </span>
          )}
          {view.compatBadge ? (
            <span
              data-tone={view.compatBadge.tone}
              className={cn(
                "shrink-0 rounded-full px-1.5 py-px text-xs font-medium",
                view.compatBadge.tone === "error"
                  ? "bg-destructive/12 text-destructive-foreground"
                  : "bg-warning/15 text-warning-foreground",
              )}
            >
              {view.compatBadge.label}
            </span>
          ) : null}
          {props.updateBadge ?? null}
        </div>
      </div>
      <Menu>
        <MenuTrigger
          render={
            <button
              type="button"
              aria-label="Environment actions"
              data-testid="environment-context-card-menu"
              className="inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            />
          }
        >
          <EllipsisIcon className="size-4" />
        </MenuTrigger>
        <MenuPopup align="end">
          <MenuItem onClick={() => void disconnectEnvironment(activeEnvironmentId)}>
            Disconnect
          </MenuItem>
          {view.manualUpdates && props.onShowUpdateSteps !== undefined ? (
            <MenuItem onClick={() => props.onShowUpdateSteps?.(activeEnvironmentId)}>
              Show update steps
            </MenuItem>
          ) : view.showUpdateActions && props.onCheckForUpdates !== undefined ? (
            <MenuItem
              disabled={props.updateInProgress}
              title={props.updateInProgress ? "An update is already queued or running." : undefined}
              onClick={() => props.onCheckForUpdates?.(activeEnvironmentId)}
            >
              Check for updates
            </MenuItem>
          ) : null}
          <MenuItem onClick={() => void navigate({ to: "/settings/remote-servers" })}>
            Manage…
          </MenuItem>
        </MenuPopup>
      </Menu>
    </div>
  );
}
