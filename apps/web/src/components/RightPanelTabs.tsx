import type { ContextMenuItem, PreviewSessionSnapshot } from "@bibcode/contracts";
import { getTerminalLabel } from "@bibcode/shared/terminalLabels";
import {
  Bot,
  ClipboardList,
  FileDiff,
  Files,
  GitPullRequestArrow,
  Globe2,
  Plus,
  Smartphone,
  TerminalSquare,
  X,
} from "lucide-react";
import {
  type MouseEvent as ReactMouseEvent,
  type ReactElement,
  type ReactNode,
  useCallback,
  useEffect,
  useRef,
} from "react";

import type { RightPanelSurface } from "~/rightPanelStore";
import type { DesktopPreviewOverlay } from "~/previewStateStore";
import { cn } from "~/lib/utils";
import { readLocalApi } from "~/localApi";
import { Tooltip, TooltipPopup, TooltipTrigger } from "~/components/ui/tooltip";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "~/components/ui/menu";
import { ScrollArea } from "~/components/ui/scroll-area";
import { useTheme } from "~/hooks/useTheme";
import { COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS } from "~/workspaceTitlebar";

import { PreviewPanelShell, type PreviewPanelMode } from "./preview/PreviewPanelShell";
import { PierreEntryIcon } from "./chat/PierreEntryIcon";

interface RightPanelTabsProps {
  mode: PreviewPanelMode;
  maximized?: boolean;
  layoutControls?: ReactNode;
  allowAddSurfaces?: boolean;
  surfaces: readonly RightPanelSurface[];
  activeSurfaceId: string | null;
  pendingSurfaceIds: ReadonlySet<string>;
  previewSessions: Readonly<Record<string, PreviewSessionSnapshot>>;
  previewDesktopByTabId: Readonly<Record<string, DesktopPreviewOverlay>>;
  terminalLabelsById: ReadonlyMap<string, string>;
  onActivate: (surface: RightPanelSurface) => void;
  onCloseSurface: (surface: RightPanelSurface) => void;
  onCloseOtherSurfaces: (surface: RightPanelSurface) => void;
  onCloseSurfacesToRight: (surface: RightPanelSurface) => void;
  onCloseAllSurfaces: () => void;
  onCopyFilePath: (relativePath: string) => void;
  onAddBrowser: () => void;
  onAddTerminal: () => void;
  onAddDiff: () => void;
  onAddFiles: () => void;
  onAddSourceControl: () => void;
  /** Opens the device pane; omitted while device previews are off or unsupported. */
  onAddDevice?: (() => void) | undefined;
  browserAvailable: boolean;
  diffAvailable: boolean;
  filesAvailable: boolean;
  sourceControlAvailable: boolean;
  children: ReactNode;
}

const SURFACE_DISABLED_REASONS = {
  browser: "Browser previews are only available in the BiBCode desktop app.",
  files: "Files are only available when a project is open.",
  diff: "Diff is only available for server threads in Git repositories.",
  sourceControl: "Source control is only available for server threads in Git repositories.",
} as const;

type TabContextMenuAction = "copy-path" | "close" | "close-others" | "close-to-right" | "close-all";

function DisabledReasonTooltip(props: { reason: string; trigger: ReactElement }) {
  return (
    <Tooltip>
      <TooltipTrigger render={props.trigger} />
      <TooltipPopup side="top">{props.reason}</TooltipPopup>
    </Tooltip>
  );
}

function SurfaceMenuItem(props: {
  available: boolean;
  disabledReason?: string;
  onClick: () => void;
  children: ReactNode;
}) {
  const item = (
    <MenuItem
      className={!props.available ? "data-disabled:pointer-events-auto" : undefined}
      onClick={props.onClick}
      disabled={!props.available}
    >
      {props.children}
    </MenuItem>
  );
  if (props.available || !props.disabledReason) return item;
  return <DisabledReasonTooltip reason={props.disabledReason} trigger={item} />;
}

function RightPanelEmptyState(props: {
  onAddBrowser: () => void;
  onAddTerminal: () => void;
  onAddDiff: () => void;
  onAddFiles: () => void;
  onAddSourceControl: () => void;
  /** Opens the device pane; omitted while device previews are off or unsupported. */
  onAddDevice?: (() => void) | undefined;
  browserAvailable: boolean;
  diffAvailable: boolean;
  filesAvailable: boolean;
  sourceControlAvailable: boolean;
}) {
  // Hidden, not disabled, while device previews are off: the setting is the way in.
  const deviceActions =
    props.onAddDevice === undefined
      ? []
      : ([
          {
            label: "Device",
            description: "Run an Android emulator.",
            icon: Smartphone,
            available: true,
            disabledReason: null,
            onClick: props.onAddDevice,
          },
        ] as const);
  const actions = [
    {
      label: "Browser",
      description: "Open a local app or URL.",
      icon: Globe2,
      available: props.browserAvailable,
      disabledReason: SURFACE_DISABLED_REASONS.browser,
      onClick: props.onAddBrowser,
    },
    {
      label: "Terminal",
      description: "Start a shell in this workspace.",
      icon: TerminalSquare,
      available: true,
      disabledReason: null,
      onClick: props.onAddTerminal,
    },
    {
      label: "Files",
      description: "Browse and read workspace files.",
      icon: Files,
      available: props.filesAvailable,
      disabledReason: SURFACE_DISABLED_REASONS.files,
      onClick: props.onAddFiles,
    },
    {
      label: "Diff",
      description: "Review changes in this thread.",
      icon: FileDiff,
      available: props.diffAvailable,
      disabledReason: SURFACE_DISABLED_REASONS.diff,
      onClick: props.onAddDiff,
    },
    {
      label: "Source Control",
      description: "Stage, commit, and open changes.",
      icon: GitPullRequestArrow,
      available: props.sourceControlAvailable,
      disabledReason: SURFACE_DISABLED_REASONS.sourceControl,
      onClick: props.onAddSourceControl,
    },
    ...deviceActions,
  ] as const;

  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-6">
      <div className="w-full max-w-xl">
        <div className="mb-5 text-center">
          <h3 className="text-sm font-medium text-foreground">Open a surface</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Choose what to show in the right panel.
          </p>
        </div>
        <div className="grid grid-cols-2 gap-2">
          {actions.map((action) => {
            const Icon = action.icon;
            const content = (
              <>
                <Icon className="mb-3 size-5" />
                <span className="text-sm font-medium">{action.label}</span>
                <span className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {action.description}
                </span>
              </>
            );
            if (action.available) {
              return (
                <button
                  key={action.label}
                  type="button"
                  onClick={action.onClick}
                  className="flex min-h-28 w-full flex-col items-start rounded-lg border border-border/80 bg-card/40 p-4 text-left transition hover:border-border hover:bg-accent/60"
                >
                  {content}
                </button>
              );
            }
            const disabledCard = (
              <button
                type="button"
                className="flex min-h-28 w-full cursor-not-allowed flex-col items-start rounded-lg border border-border/80 bg-card/40 p-4 text-left opacity-40"
                aria-disabled="true"
              >
                {content}
              </button>
            );
            return (
              <DisabledReasonTooltip
                key={action.label}
                reason={action.disabledReason}
                trigger={disabledCard}
              />
            );
          })}
        </div>
      </div>
    </div>
  );
}

function surfaceTitle(
  surface: RightPanelSurface,
  sessions: Readonly<Record<string, PreviewSessionSnapshot>>,
  desktopByTabId: Readonly<Record<string, DesktopPreviewOverlay>>,
  terminalLabelsById: ReadonlyMap<string, string>,
): string {
  switch (surface.kind) {
    case "activity":
      return "Activity";
    case "diff":
      return "Diff";
    case "sourceControl":
      return "Source Control";
    case "files":
      return "Files";
    case "file":
      return surface.relativePath.slice(surface.relativePath.lastIndexOf("/") + 1);
    case "terminal":
      return (
        terminalLabelsById.get(surface.activeTerminalId) ??
        getTerminalLabel(surface.activeTerminalId)
      );
    case "plan":
      return "Plan";
    case "device":
      return "Device";
    case "preview": {
      const snapshot = surface.resourceId ? sessions[surface.resourceId] : null;
      const desktop = surface.resourceId ? desktopByTabId[surface.resourceId] : null;
      const title =
        desktop?.title ??
        (snapshot && snapshot.navStatus._tag !== "Idle" ? snapshot.navStatus.title : "");
      const url =
        desktop?.url ??
        (snapshot && snapshot.navStatus._tag !== "Idle" ? snapshot.navStatus.url : null);
      if (title.trim().length > 0) return title;
      if (!url) return "Browser";
      try {
        return new URL(url).host || "Browser";
      } catch {
        return "Browser";
      }
    }
  }
}

function SurfaceIcon({ surface, theme }: { surface: RightPanelSurface; theme: "light" | "dark" }) {
  switch (surface.kind) {
    case "activity":
      return <Bot className="size-3.5 shrink-0" />;
    case "preview":
      return <Globe2 className="size-3.5 shrink-0" />;
    case "diff":
      return <FileDiff className="size-3.5 shrink-0" />;
    case "sourceControl":
      return <GitPullRequestArrow className="size-3.5 shrink-0" />;
    case "files":
      return <Files className="size-3.5 shrink-0" />;
    case "file":
      return (
        <PierreEntryIcon
          pathValue={surface.relativePath}
          kind="file"
          theme={theme}
          className="size-3.5"
        />
      );
    case "terminal":
      return <TerminalSquare className="size-3.5 shrink-0" />;
    case "plan":
      return <ClipboardList className="size-3.5 shrink-0" />;
    case "device":
      return <Smartphone className="size-3.5 shrink-0" />;
  }
}

export function RightPanelTabs(props: RightPanelTabsProps) {
  const { resolvedTheme } = useTheme();
  const tabListRef = useRef<HTMLDivElement>(null);

  const handleTabContextMenu = useCallback(
    async (event: ReactMouseEvent, surface: RightPanelSurface) => {
      event.preventDefault();
      event.stopPropagation();

      const api = readLocalApi();
      if (!api) return;

      const surfaceIndex = props.surfaces.findIndex((entry) => entry.id === surface.id);
      if (surfaceIndex < 0) return;

      const items: ContextMenuItem<TabContextMenuAction>[] = [];
      if (surface.kind === "file") {
        items.push({ id: "copy-path", label: "Copy path" });
      }
      items.push(
        { id: "close", label: "Close" },
        {
          id: "close-others",
          label: "Close others",
          disabled: props.surfaces.length <= 1,
        },
        {
          id: "close-to-right",
          label: "Close to the right",
          disabled: surfaceIndex >= props.surfaces.length - 1,
        },
        {
          id: "close-all",
          label: "Close all",
          disabled: props.surfaces.length === 0,
        },
      );

      const action = await api.contextMenu.show(items, { x: event.clientX, y: event.clientY });
      switch (action) {
        case "copy-path":
          if (surface.kind === "file") props.onCopyFilePath(surface.relativePath);
          break;
        case "close":
          props.onCloseSurface(surface);
          break;
        case "close-others":
          props.onCloseOtherSurfaces(surface);
          break;
        case "close-to-right":
          props.onCloseSurfacesToRight(surface);
          break;
        case "close-all":
          props.onCloseAllSurfaces();
          break;
        case null:
          break;
      }
    },
    [props],
  );
  const handleTabMouseDown = useCallback((event: ReactMouseEvent) => {
    if (event.button !== 1) return;
    event.preventDefault();
  }, []);
  const handleTabAuxClick = useCallback(
    (event: ReactMouseEvent, surface: RightPanelSurface) => {
      if (event.button !== 1) return;
      event.preventDefault();
      event.stopPropagation();
      props.onCloseSurface(surface);
    },
    [props],
  );

  useEffect(() => {
    const activeTab = tabListRef.current?.querySelector<HTMLElement>("[data-active-tab='true']");
    activeTab?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [props.activeSurfaceId]);

  return (
    <PreviewPanelShell
      mode={props.mode}
      {...(props.maximized !== undefined ? { maximized: props.maximized } : {})}
    >
      <div
        className={cn(
          "workspace-topbar gap-1 pl-2",
          props.mode === "inline" ? "pr-28" : "pr-3",
          props.mode === "inline" && props.maximized && COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS,
        )}
        data-right-panel-tabbar
      >
        <ScrollArea
          ref={tabListRef}
          hideScrollbars
          scrollFade
          className="min-w-0 flex-1 rounded-none"
          data-right-panel-tab-list
        >
          <div className="flex h-full w-max min-w-full items-center gap-1">
            {props.surfaces.map((surface) => {
              const active = surface.id === props.activeSurfaceId;
              const pending = props.pendingSurfaceIds.has(surface.id);
              const title = surfaceTitle(
                surface,
                props.previewSessions,
                props.previewDesktopByTabId,
                props.terminalLabelsById,
              );
              const tabButton = (
                <button
                  type="button"
                  className="flex min-w-0 flex-1 items-center gap-1.5"
                  onClick={() => props.onActivate(surface)}
                >
                  <SurfaceIcon surface={surface} theme={resolvedTheme} />
                  <span className="truncate">{title}</span>
                </button>
              );
              return (
                <div
                  key={surface.id}
                  data-active-tab={active}
                  onMouseDown={handleTabMouseDown}
                  onAuxClick={(event) => handleTabAuxClick(event, surface)}
                  onContextMenu={(event) => void handleTabContextMenu(event, surface)}
                  className={cn(
                    "group flex h-7 min-w-25 max-w-44 shrink-0 items-center gap-1.5 rounded-md px-2 text-sm",
                    active
                      ? "bg-accent text-foreground"
                      : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
                  )}
                >
                  {surface.kind === "activity" ? (
                    tabButton
                  ) : (
                    <Tooltip>
                      <TooltipTrigger render={tabButton} />
                      <TooltipPopup>{title}</TooltipPopup>
                    </Tooltip>
                  )}
                  <button
                    type="button"
                    className={cn(
                      "relative flex size-4 shrink-0 items-center justify-center rounded hover:bg-muted focus:opacity-100",
                      pending ? "opacity-100" : "opacity-0 group-hover:opacity-100",
                    )}
                    aria-label={`Close ${title}`}
                    onClick={() => props.onCloseSurface(surface)}
                  >
                    {pending ? (
                      <>
                        <span
                          className="size-2 rounded-full bg-current group-hover:hidden"
                          aria-hidden
                        />
                        <X className="hidden size-3 group-hover:block" />
                      </>
                    ) : (
                      <X className="size-3" />
                    )}
                  </button>
                </div>
              );
            })}
            {props.surfaces.length > 0 && props.allowAddSurfaces !== false ? (
              <Menu>
                <MenuTrigger
                  className="relative inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
                  aria-label="Add panel surface"
                >
                  <Plus className="size-4" />
                </MenuTrigger>
                <MenuPopup align="start" side="bottom" sideOffset={6} className="min-w-44">
                  <SurfaceMenuItem
                    available={props.browserAvailable}
                    disabledReason={SURFACE_DISABLED_REASONS.browser}
                    onClick={props.onAddBrowser}
                  >
                    <Globe2 />
                    Browser
                  </SurfaceMenuItem>
                  <SurfaceMenuItem available onClick={props.onAddTerminal}>
                    <TerminalSquare />
                    Terminal
                  </SurfaceMenuItem>
                  <SurfaceMenuItem
                    available={props.filesAvailable}
                    disabledReason={SURFACE_DISABLED_REASONS.files}
                    onClick={props.onAddFiles}
                  >
                    <Files />
                    Files
                  </SurfaceMenuItem>
                  <SurfaceMenuItem
                    available={props.diffAvailable}
                    disabledReason={SURFACE_DISABLED_REASONS.diff}
                    onClick={props.onAddDiff}
                  >
                    <FileDiff />
                    Diff
                  </SurfaceMenuItem>
                  <SurfaceMenuItem
                    available={props.sourceControlAvailable}
                    disabledReason={SURFACE_DISABLED_REASONS.sourceControl}
                    onClick={props.onAddSourceControl}
                  >
                    <GitPullRequestArrow />
                    Source Control
                  </SurfaceMenuItem>
                  {props.onAddDevice === undefined ? null : (
                    <SurfaceMenuItem available onClick={props.onAddDevice}>
                      <Smartphone />
                      Device
                    </SurfaceMenuItem>
                  )}
                </MenuPopup>
              </Menu>
            ) : null}
          </div>
        </ScrollArea>
        {props.layoutControls}
      </div>
      <div className="flex min-h-0 flex-1 flex-col">
        {props.activeSurfaceId === null ? (
          <RightPanelEmptyState
            onAddBrowser={props.onAddBrowser}
            onAddTerminal={props.onAddTerminal}
            onAddDiff={props.onAddDiff}
            onAddFiles={props.onAddFiles}
            onAddSourceControl={props.onAddSourceControl}
            onAddDevice={props.onAddDevice}
            browserAvailable={props.browserAvailable}
            diffAvailable={props.diffAvailable}
            filesAvailable={props.filesAvailable}
            sourceControlAvailable={props.sourceControlAvailable}
          />
        ) : (
          props.children
        )}
      </div>
    </PreviewPanelShell>
  );
}
