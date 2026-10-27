import type { ProviderDriverKind } from "@coddy/contracts";
import {
  CircleAlertIcon,
  CircleHelpIcon,
  GitBranchIcon,
  Globe2Icon,
  HandIcon,
  ListChecksIcon,
  LoaderCircleIcon,
  PinIcon,
  TerminalIcon,
  TriangleAlertIcon,
} from "lucide-react";
import type * as React from "react";
import type { ReactNode } from "react";

import { cn } from "../../lib/utils";
import { ProviderInstanceIcon } from "../chat/ProviderInstanceIcon";
import type { WorkspaceCardStatus } from "../Sidebar.logic";
import { ChangeRequestStatusIcon, type PrStatusIndicator } from "../ThreadStatusIndicators";
import { SidebarMenuSubItem } from "../ui/sidebar";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { RelativeAge } from "./RelativeAge";
import type { WorkspaceCardPreview } from "./workspaceCard.logic";

/** Element ids a card's button points at for its name and description. */
export function workspaceCardIds(idBase: string) {
  return {
    status: `${idBase}-status`,
    title: `${idBase}-title`,
    flags: `${idBase}-flags`,
    branch: `${idBase}-branch`,
    session: `${idBase}-session`,
  } as const;
}

/** True for events that start on one of the card's own controls (PR, ports, Archive, rename). */
export function isWorkspaceCardControlTarget(target: EventTarget | null): boolean {
  const element = target as { closest?: (selector: string) => unknown } | null;
  return typeof element?.closest === "function" && element.closest("[data-card-control]") != null;
}

const GLYPH_ICON_CLASS = "size-3.5";

function StatusShape({ kind }: { readonly kind: WorkspaceCardStatus["kind"] }) {
  switch (kind) {
    case "approval":
      return <HandIcon aria-hidden className={GLYPH_ICON_CLASS} />;
    case "input":
      return <CircleHelpIcon aria-hidden className={GLYPH_ICON_CLASS} />;
    case "working":
      return (
        <LoaderCircleIcon
          aria-hidden
          className={cn(GLYPH_ICON_CLASS, "motion-safe:animate-spin")}
        />
      );
    case "failed":
      return <TriangleAlertIcon aria-hidden className={GLYPH_ICON_CLASS} />;
    case "plan":
      return <ListChecksIcon aria-hidden className={GLYPH_ICON_CLASS} />;
    case "done":
      return (
        <svg aria-hidden viewBox="0 0 10 10" className="size-2.5">
          <circle cx="5" cy="5" r="4" fill="currentColor" />
        </svg>
      );
    case "idle":
      return (
        <svg aria-hidden viewBox="0 0 10 10" className="size-2.5">
          <circle cx="5" cy="5" r="3.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
        </svg>
      );
  }
}

/** The status glyph: its shape carries the meaning and colour only helps (UI.md:208-210). */
export function WorkspaceCardStatusGlyph(props: {
  readonly status: WorkspaceCardStatus;
  readonly id?: string;
  readonly className?: string;
}) {
  const { status } = props;
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            role="img"
            aria-label={status.label}
            id={props.id}
            data-status={status.kind}
            className={cn(
              "pointer-events-auto inline-flex items-center justify-center",
              status.colorClass,
              props.className,
            )}
          />
        }
      >
        <StatusShape kind={status.kind} />
      </TooltipTrigger>
      <TooltipPopup side="top">{status.label}</TooltipPopup>
    </Tooltip>
  );
}

export interface WorkspaceCardShellProps {
  readonly testId: string;
  readonly buttonTestId: string;
  readonly className: string;
  readonly idBase: string;
  readonly isActive: boolean;
  readonly hasFlags: boolean;
  readonly hasBranchLine: boolean;
  readonly hasSessionLine: boolean;
  readonly status: WorkspaceCardStatus;
  readonly children: ReactNode;
  readonly footer?: ReactNode;
  readonly onClick: (event: React.MouseEvent<HTMLLIElement>) => void;
  readonly onDoubleClick?: (event: React.MouseEvent<HTMLLIElement>) => void;
  readonly onContextMenu: (event: React.MouseEvent<HTMLLIElement>) => void;
  readonly onMouseLeave?: () => void;
  readonly onBlurCapture?: (event: React.FocusEvent<HTMLLIElement>) => void;
  readonly onButtonKeyDown: (event: React.KeyboardEvent<HTMLButtonElement>) => void;
}

/**
 * A workspace card: an `li` holding one `<button>` that covers the whole card
 * and carries its name (status, title, unread/pinned) and description (lines
 * 2–3). The visible content sits above the button and lets pointer events fall
 * through to it, except the card's own controls, which are siblings of the
 * button and never inside it. Pointer handlers live on the `li`, so a click
 * anywhere on the card, or Enter/Space on the focused button, reaches them.
 */
export function WorkspaceCardShell(props: WorkspaceCardShellProps) {
  const ids = workspaceCardIds(props.idBase);
  const labelledBy = [ids.status, ids.title, ...(props.hasFlags ? [ids.flags] : [])].join(" ");
  const describedBy = [
    ...(props.hasBranchLine ? [ids.branch] : []),
    ...(props.hasSessionLine ? [ids.session] : []),
  ].join(" ");
  return (
    <SidebarMenuSubItem
      className={props.className}
      data-thread-item
      data-testid={props.testId}
      onClick={props.onClick}
      onDoubleClick={props.onDoubleClick}
      onContextMenu={props.onContextMenu}
      onMouseLeave={props.onMouseLeave}
      onBlurCapture={props.onBlurCapture}
    >
      <button
        type="button"
        data-testid={props.buttonTestId}
        aria-labelledby={labelledBy}
        aria-describedby={describedBy === "" ? undefined : describedBy}
        aria-current={props.isActive ? "page" : undefined}
        className="absolute inset-0 z-0 cursor-pointer rounded-md outline-hidden focus-visible:ring-2 focus-visible:ring-ring"
        onKeyDown={props.onButtonKeyDown}
      />
      <div className="pointer-events-none relative z-10 flex gap-2 px-2 py-1.5">
        <span className="flex h-5 w-4 shrink-0 items-center justify-center">
          <WorkspaceCardStatusGlyph status={props.status} id={ids.status} />
        </span>
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">{props.children}</div>
      </div>
      {props.footer ? (
        <div className="relative z-10" data-card-control>
          {props.footer}
        </div>
      ) : null}
    </SidebarMenuSubItem>
  );
}

/** Line 1: title (bold while unread), primary chip, pin, then trailing controls. */
export function WorkspaceCardTitleLine(props: {
  readonly id: string;
  readonly flagsId: string;
  readonly title: string;
  readonly titleTestId: string;
  readonly unread: boolean;
  readonly pinned: boolean;
  readonly pinnedTestId?: string;
  readonly primary?: boolean;
  /** Replaces the visible title while renaming inline. */
  readonly renameInput?: ReactNode;
  /** Archive/Confirm, or the ⌘ jump label while the modifier is held. */
  readonly trailing?: ReactNode;
}) {
  const flags = [props.unread ? "unread" : null, props.pinned ? "pinned" : null]
    .filter((flag) => flag !== null)
    .join(", ");
  return (
    <div className="flex h-5 min-w-0 items-center gap-1.5">
      {props.renameInput ? (
        <>
          <span id={props.id} className="sr-only">
            {props.title}
          </span>
          {props.renameInput}
        </>
      ) : (
        <Tooltip>
          <TooltipTrigger
            render={
              <span
                id={props.id}
                data-testid={props.titleTestId}
                data-unread={props.unread ? "true" : undefined}
                className={cn(
                  "pointer-events-auto min-w-0 truncate text-[13px]",
                  props.unread ? "font-semibold text-foreground" : "text-foreground/80",
                )}
              />
            }
          >
            {props.title}
          </TooltipTrigger>
          <TooltipPopup side="top" className="max-w-80 whitespace-normal leading-tight">
            {props.title}
          </TooltipPopup>
        </Tooltip>
      )}
      {props.primary ? (
        <span className="shrink-0 rounded border border-border bg-muted px-1.5 text-xs leading-4 text-muted-foreground">
          primary
        </span>
      ) : null}
      {props.pinned ? (
        <PinIcon
          aria-hidden
          data-testid={props.pinnedTestId}
          className="size-3 shrink-0 text-muted-foreground"
        />
      ) : null}
      {flags ? (
        <span id={props.flagsId} className="sr-only">
          {flags}
        </span>
      ) : null}
      <span className="flex-1" />
      {props.trailing}
    </div>
  );
}

/** Line 2: branch, notice, or spacer, then indicators at the right end. */
export function WorkspaceCardBranchLine(props: {
  readonly id: string;
  readonly branch: string | null;
  readonly branchTooltip: string | null;
  readonly notice?: { readonly label: string; readonly description: string } | null;
  readonly children?: ReactNode;
}) {
  return (
    <div
      id={props.id}
      className="flex h-[18px] min-w-0 items-center gap-1.5 text-xs text-muted-foreground"
    >
      {props.notice ? (
        <>
          <CircleAlertIcon aria-hidden className="size-3 shrink-0" />
          <Tooltip>
            <TooltipTrigger
              render={<span aria-hidden className="pointer-events-auto min-w-0 flex-1 truncate" />}
            >
              {props.notice.label}
            </TooltipTrigger>
            <TooltipPopup side="top">{props.notice.description}</TooltipPopup>
          </Tooltip>
          <span className="sr-only">{props.notice.description}</span>
        </>
      ) : props.branch !== null ? (
        <>
          <GitBranchIcon aria-hidden className="size-3 shrink-0" />
          <Tooltip>
            <TooltipTrigger
              render={<span className="pointer-events-auto min-w-0 flex-1 truncate" />}
            >
              {props.branch}
            </TooltipTrigger>
            {props.branchTooltip ? (
              <TooltipPopup side="top">{props.branchTooltip}</TooltipPopup>
            ) : null}
          </Tooltip>
        </>
      ) : (
        <span className="flex-1" />
      )}
      {props.children}
    </div>
  );
}

/** The PR/MR number: a button coloured by its state that opens the request. */
export function WorkspaceCardPrButton(props: {
  readonly indicator: PrStatusIndicator;
  readonly onClick: (event: React.MouseEvent<HTMLButtonElement>) => void;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            data-card-control
            aria-label={props.indicator.tooltip}
            className={cn(
              "pointer-events-auto inline-flex min-h-6 min-w-6 shrink-0 cursor-pointer items-center justify-center gap-[3px] rounded-sm outline-hidden focus-visible:ring-1 focus-visible:ring-ring",
              props.indicator.colorClass,
            )}
            onClick={props.onClick}
          />
        }
      >
        <ChangeRequestStatusIcon className="size-[13px]" />
        {props.indicator.numberLabel}
      </TooltipTrigger>
      <TooltipPopup side="top">{props.indicator.tooltip}</TooltipPopup>
    </Tooltip>
  );
}

export function WorkspaceCardDirtyDot() {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            role="img"
            aria-label="Uncommitted changes"
            className="pointer-events-auto size-1.5 shrink-0 rounded-full bg-warning"
          />
        }
      />
      <TooltipPopup side="top">Uncommitted changes</TooltipPopup>
    </Tooltip>
  );
}

export function WorkspaceCardTerminalIcon(props: {
  readonly label: string;
  readonly colorClass: string;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            role="img"
            aria-label={props.label}
            className={cn(
              "pointer-events-auto inline-flex shrink-0 items-center justify-center",
              props.colorClass,
            )}
          />
        }
      >
        <TerminalIcon aria-hidden className="size-[13px]" />
      </TooltipTrigger>
      <TooltipPopup side="top">{props.label}</TooltipPopup>
    </Tooltip>
  );
}

/** Opens the first local server a terminal of this card discovered. */
export function WorkspaceCardPortsButton(props: {
  readonly ports: ReadonlyArray<{ readonly port: number }>;
  readonly onClick: (event: React.MouseEvent<HTMLButtonElement>) => void;
}) {
  const first = props.ports[0];
  if (!first) {
    return null;
  }
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            data-card-control
            aria-label={`Open localhost:${first.port}`}
            className="pointer-events-auto inline-flex min-h-6 min-w-6 shrink-0 cursor-pointer items-center justify-center text-emerald-600 outline-hidden focus-visible:ring-1 focus-visible:ring-ring dark:text-emerald-400"
            onClick={props.onClick}
          />
        }
      >
        <Globe2Icon aria-hidden className="size-[13px]" />
      </TooltipTrigger>
      <TooltipPopup side="top">
        Open localhost:{first.port}
        {props.ports.length > 1 ? ` (+${props.ports.length - 1})` : ""}
      </TooltipPopup>
    </Tooltip>
  );
}

/** Line 3: provider icon, preview, model and age. */
export function WorkspaceCardSessionLine(props: {
  readonly id: string;
  readonly provider: {
    readonly driverKind: ProviderDriverKind | null;
    readonly label: string | null;
  };
  readonly preview: WorkspaceCardPreview;
  readonly model: string;
  readonly ageIso: string | null;
}) {
  return (
    <div
      id={props.id}
      className="flex h-[22px] min-w-0 items-center gap-1.5 text-xs text-muted-foreground"
    >
      {props.provider.driverKind !== null ? (
        <ProviderInstanceIcon
          driverKind={props.provider.driverKind}
          displayName={props.provider.label ?? ""}
          className="workspace-card-provider-icon size-[13px]"
          iconClassName="size-[13px] text-xs"
          showBadge={false}
        />
      ) : null}
      <span
        className={cn(
          "min-w-0 flex-1 truncate",
          props.preview.tone === "destructive" && "text-destructive",
          props.preview.tone === "warning" && "text-warning-foreground",
        )}
      >
        {props.preview.text}
      </span>
      <span className="max-w-28 shrink-0 truncate font-mono text-xs">{props.model}</span>
      <RelativeAge iso={props.ageIso} className="shrink-0 tabular-nums" />
    </div>
  );
}

/** Other chats open in this checkout, until the server links chats to their host thread. */
export function WorkspaceCardMoreChats(props: {
  readonly count: number;
  readonly status: WorkspaceCardStatus;
}) {
  return (
    <div className="flex h-5 min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
      <span className="flex w-4 shrink-0 items-center justify-center">
        <WorkspaceCardStatusGlyph status={props.status} />
      </span>
      <span className="truncate">
        {props.count === 1 ? "1 more chat" : `${props.count} more chats`}
      </span>
    </div>
  );
}
