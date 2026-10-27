import { useNavigate } from "@tanstack/react-router";
import { MonitorIcon, PlusIcon, Settings2Icon } from "lucide-react";
import * as React from "react";
import { useCallback, useEffect, useMemo } from "react";

import type { EnvironmentId } from "@coddy/contracts";
import { isRemoteUpdateAvailable } from "@coddy/client-runtime/state/remoteUpdates";

import {
  resolveEnvironmentCompatVerdict,
  selectRemoteUpdateControlCapability,
} from "../../connection/environmentCompat";
import { cn } from "../../lib/utils";
import { setActiveEnvironmentId, useActiveEnvironmentId } from "../../state/entities";
import { type EnvironmentPresentation, useEnvironments } from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { remoteUpdateEnvironment } from "../../state/remoteUpdates";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  buildEnvironmentRailModel,
  ENVIRONMENT_RAIL_WIDTH_PX,
  resolveEnvironmentRailStatus,
  toEnvironmentRailCandidate,
  type EnvironmentRailEntry,
  type EnvironmentRailStatus,
} from "./environmentRail.logic";

const STATUS_DOT_CLASS: Record<EnvironmentRailStatus, string> = {
  connected: "bg-success",
  disconnected: "bg-muted-foreground/50",
  attention: "bg-warning",
  error: "bg-destructive",
};

const RAIL_BUTTON_CLASS =
  "relative flex size-9 items-center justify-center rounded-[10px] text-muted-foreground outline-hidden transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

// The active host must read at a glance: a full-strength primary ring around
// the rounded square (the app's selected-state vocabulary), a solid accent
// fill, foreground-strength icon, and the edge bar. The unselected entries stay
// muted so the ring is the only framed element in the rail. No ring offset:
// the rail's `bg-sidebar` is not a theme color token, so an offset would paint
// its gap in the default ring-offset color.
const RAIL_BUTTON_SELECTED_CLASS =
  "bg-accent text-foreground ring-2 ring-primary before:absolute before:top-2 before:bottom-2 before:-left-2 before:w-[3px] before:rounded-full before:bg-primary";

export function EnvironmentStatusDot({
  status,
  className = "absolute right-0.5 bottom-0.5",
}: {
  readonly status: EnvironmentRailStatus;
  readonly className?: string;
}) {
  return (
    <span
      data-status={status}
      className={cn(
        "size-2 rounded-full border-2 border-sidebar",
        className,
        STATUS_DOT_CLASS[status],
      )}
    />
  );
}

function RemoteEntryButton({
  entry,
  onSelect,
}: {
  readonly entry: EnvironmentRailEntry;
  readonly onSelect: (environmentId: EnvironmentId) => void;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            role="radio"
            aria-checked={entry.selected}
            data-selected={entry.selected}
            tabIndex={entry.selected ? 0 : -1}
            aria-label={entry.label}
            data-testid={`environment-rail-entry-${entry.environmentId}`}
            className={cn(RAIL_BUTTON_CLASS, entry.selected && RAIL_BUTTON_SELECTED_CLASS)}
            onClick={() => onSelect(entry.environmentId)}
          />
        }
      >
        <span
          className={cn(
            "flex size-[26px] items-center justify-center rounded-lg text-xs font-semibold",
            entry.selected
              ? "bg-primary text-primary-foreground"
              : "bg-muted text-muted-foreground",
          )}
        >
          {entry.avatar}
        </span>
        <EnvironmentStatusDot status={entry.status} />
      </TooltipTrigger>
      <TooltipPopup side="right">{entry.label}</TooltipPopup>
    </Tooltip>
  );
}

function RemoteEntryButtonWithUpdate({
  entry,
  environment,
  onSelect,
}: {
  readonly entry: EnvironmentRailEntry;
  readonly environment: EnvironmentPresentation;
  readonly onSelect: (environmentId: EnvironmentId) => void;
}) {
  const remoteUpdateControl = selectRemoteUpdateControlCapability(environment.serverConfig);
  const updateQuery = useEnvironmentQuery(
    remoteUpdateControl
      ? remoteUpdateEnvironment.snapshot({
          environmentId: environment.environmentId,
          input: {},
        })
      : null,
  );
  const candidate = toEnvironmentRailCandidate({
    environmentId: environment.environmentId,
    label: environment.label,
    target: environment.entry.target,
    phase: environment.connection.phase,
    compat: resolveEnvironmentCompatVerdict(environment.serverConfig),
    updateAvailable: isRemoteUpdateAvailable(updateQuery.data),
  });

  return (
    <RemoteEntryButton
      entry={{ ...entry, status: resolveEnvironmentRailStatus(candidate) }}
      onSelect={onSelect}
    />
  );
}

/**
 * Compact environment selector. Selection only changes panel presentation;
 * connection desired state and entity-owned routing remain untouched.
 */
export function EnvironmentRail() {
  const { environments, isReady } = useEnvironments();
  const activeEnvironmentId = useActiveEnvironmentId();
  const navigate = useNavigate();

  const candidates = useMemo(
    () =>
      environments.map((environment) =>
        toEnvironmentRailCandidate({
          environmentId: environment.environmentId,
          label: environment.label,
          target: environment.entry.target,
          phase: environment.connection.phase,
          compat: resolveEnvironmentCompatVerdict(environment.serverConfig),
          updateAvailable: false,
        }),
      ),
    [environments],
  );
  const model = useMemo(
    () => buildEnvironmentRailModel({ candidates, activeEnvironmentId }),
    [activeEnvironmentId, candidates],
  );

  const selectEnvironment = useCallback((environmentId: EnvironmentId) => {
    setActiveEnvironmentId(environmentId);
  }, []);

  useEffect(() => {
    if (!isReady || activeEnvironmentId === null) {
      return;
    }
    if (candidates.some((candidate) => candidate.environmentId === activeEnvironmentId)) {
      return;
    }
    setActiveEnvironmentId(model.localTargetEnvironmentId);
  }, [activeEnvironmentId, candidates, isReady, model.localTargetEnvironmentId]);

  const handleRadioKeyDown = useCallback((event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      return;
    }
    const radios = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="radio"]'),
    );
    if (radios.length === 0) {
      return;
    }
    const currentIndex = radios.findIndex((radio) => radio === document.activeElement);
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? radios.length - 1
          : event.key === "ArrowDown"
            ? (currentIndex + 1 + radios.length) % radios.length
            : (currentIndex - 1 + radios.length) % radios.length;
    event.preventDefault();
    radios[nextIndex]?.focus();
  }, []);

  const localButtonProps = {
    type: "button" as const,
    role: "radio" as const,
    "aria-checked": model.localSelected,
    "data-selected": model.localSelected,
    tabIndex: model.localSelected ? 0 : -1,
    "aria-label": "Local — this machine",
    "data-testid": "environment-rail-local",
    className: cn(RAIL_BUTTON_CLASS, model.localSelected && RAIL_BUTTON_SELECTED_CLASS),
  };

  return (
    <div
      data-testid="environment-rail"
      style={{ width: ENVIRONMENT_RAIL_WIDTH_PX }}
      className="flex h-full shrink-0 flex-col items-center gap-2 border-r border-panel-separator bg-sidebar pb-2"
    >
      {/* The fixed sidebar toggle is pinned over the rail's top strip; reserve
          the same topbar height the thread sidebar header reserves so the
          Local entry starts below it and the separator line stays continuous. */}
      <div
        data-testid="environment-rail-topbar"
        aria-hidden
        className="workspace-topbar w-full shrink-0 border-b border-panel-separator"
      />
      <div
        role="radiogroup"
        aria-label="Environments"
        className="flex flex-col items-center gap-2"
        onKeyDown={handleRadioKeyDown}
      >
        {model.localSubEntries.length > 0 ? (
          <Menu>
            <MenuTrigger render={<button {...localButtonProps} />}>
              <MonitorIcon className="size-[18px]" />
              <EnvironmentStatusDot status={model.localStatus} />
            </MenuTrigger>
            <MenuPopup side="right" align="start">
              {model.localSubEntries.map((entry) => (
                <MenuItem
                  key={entry.environmentId}
                  onClick={() => selectEnvironment(entry.environmentId)}
                >
                  {entry.label}
                </MenuItem>
              ))}
            </MenuPopup>
          </Menu>
        ) : (
          <Tooltip>
            <TooltipTrigger
              render={
                <button
                  {...localButtonProps}
                  onClick={() => {
                    if (model.localTargetEnvironmentId !== null) {
                      selectEnvironment(model.localTargetEnvironmentId);
                    }
                  }}
                />
              }
            >
              <MonitorIcon className="size-[18px]" />
              <EnvironmentStatusDot status={model.localStatus} />
            </TooltipTrigger>
            <TooltipPopup side="right">Local — this machine</TooltipPopup>
          </Tooltip>
        )}
        {model.remotes.length > 0 ? (
          <div
            data-testid="environment-rail-divider"
            role="presentation"
            className="h-px w-6 bg-border"
          />
        ) : null}
        {model.remotes.map((entry) => {
          const environment = environments.find(
            (candidate) => candidate.environmentId === entry.environmentId,
          );
          return environment === undefined ? null : (
            <RemoteEntryButtonWithUpdate
              key={entry.environmentId}
              entry={entry}
              environment={environment}
              onSelect={selectEnvironment}
            />
          );
        })}
      </div>
      <div className="flex-1" />
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              aria-label="Add server…"
              data-testid="environment-rail-add-server"
              className={RAIL_BUTTON_CLASS}
              onClick={() =>
                void navigate({
                  to: "/settings/remote-servers",
                  search: { action: "add-server" },
                })
              }
            />
          }
        >
          <PlusIcon className="size-[18px]" />
        </TooltipTrigger>
        <TooltipPopup side="right">Add server…</TooltipPopup>
      </Tooltip>
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              aria-label="Manage remote servers…"
              data-testid="environment-rail-manage"
              className={RAIL_BUTTON_CLASS}
              onClick={() => void navigate({ to: "/settings/remote-servers" })}
            />
          }
        >
          <Settings2Icon className="size-[18px]" />
        </TooltipTrigger>
        <TooltipPopup side="right">Manage remote servers…</TooltipPopup>
      </Tooltip>
    </div>
  );
}
