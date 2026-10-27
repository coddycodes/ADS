"use client";

/* oxlint-disable react/no-unstable-nested-components */

import { scopeProjectRef, scopeThreadRef } from "@coddy/client-runtime/environment";
import { type ProjectId } from "@coddy/contracts";
import { useNavigate, useParams } from "@tanstack/react-router";
import {
  ArrowDownIcon,
  ArrowLeftIcon,
  ArrowUpIcon,
  FolderPlusIcon,
  MessageSquareIcon,
  SettingsIcon,
  SmartphoneIcon,
  SquarePenIcon,
} from "lucide-react";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useAtomValue } from "@effect/atom-react";
import { OpenAddProjectCommandPaletteProvider } from "../commandPaletteContext";
import { isDesktopLocalConnectionTarget } from "../connection/desktopLocal";
import { useHandleNewThread } from "../hooks/useHandleNewThread";
import { useClientSettings } from "../hooks/useSettings";
import { useActiveEnvironmentId, useProjects, useThreadShells } from "../state/entities";
import { useEnvironments } from "../state/environments";
import {
  startNewThreadInProjectFromContext,
  startNewThreadFromContext,
} from "../lib/chatThreadActions";
import { isTerminalFocused } from "../lib/terminalFocus";
import { getLatestThreadForProject } from "../lib/threadSort";
import { cn } from "../lib/utils";
import { useThreadHasTerminalSurface } from "../terminalSurfaceState";
import { buildThreadRouteParams, resolveThreadRouteTarget } from "../threadRoutes";
import {
  ADDON_ICON_CLASS,
  buildProjectActionItems,
  buildRootGroups,
  buildThreadActionItems,
  type CommandPaletteActionItem,
  type CommandPaletteSubmenuItem,
  type CommandPaletteView,
  filterCommandPaletteGroups,
  getCommandPaletteInputPlaceholder,
  getCommandPaletteMode,
  ITEM_ICON_CLASS,
  RECENT_THREAD_LIMIT,
} from "./CommandPalette.logic";
import { AddProjectDialog } from "./AddProjectDialog";
import { CommandPaletteResults } from "./CommandPaletteResults";
import { ProjectFavicon } from "./ProjectFavicon";
import { ThreadRowLeadingStatus, ThreadRowTrailingStatus } from "./ThreadStatusIndicators";
import { primaryServerKeybindingsAtom } from "../state/server";
import { resolveShortcutCommand } from "../keybindings";
import { useRightPanelStore } from "../rightPanelStore";
import { useDevicePreviewsAvailability } from "./device/deviceAvailability";
import {
  Command,
  CommandDialog,
  CommandDialogPopup,
  CommandFooter,
  CommandInput,
  CommandPanel,
} from "./ui/command";
import { Kbd, KbdGroup } from "./ui/kbd";
import { stackedThreadToast, toastManager } from "./ui/toast";
import { ComposerHandleContext, useComposerHandleContext } from "../composerHandleContext";
import type { ChatComposerHandle } from "./chat/ChatComposer";
import { resolveAddProjectTargetLabel } from "./sidebar/environmentRail.logic";

interface CommandPaletteOpenIntent {
  readonly kind: "add-project";
}

export interface CommandPaletteUiState {
  readonly open: boolean;
  readonly openIntent: CommandPaletteOpenIntent | null;
}

export type CommandPaletteUiAction =
  | { readonly _tag: "SetOpen"; readonly open: boolean }
  | { readonly _tag: "Toggle" }
  | { readonly _tag: "OpenAddProject" }
  | { readonly _tag: "ClearOpenIntent" };

export function reduceCommandPaletteUiState(
  state: CommandPaletteUiState,
  action: CommandPaletteUiAction,
): CommandPaletteUiState {
  switch (action._tag) {
    case "SetOpen":
      return {
        open: action.open,
        openIntent: action.open ? state.openIntent : null,
      };
    case "Toggle":
      return { open: !state.open, openIntent: null };
    case "OpenAddProject":
      return { open: true, openIntent: { kind: "add-project" } };
    case "ClearOpenIntent":
      return state.openIntent ? { ...state, openIntent: null } : state;
  }
}

export function CommandPalette({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reduceCommandPaletteUiState, {
    open: false,
    openIntent: null,
  });
  const setOpen = useCallback((open: boolean) => dispatch({ _tag: "SetOpen", open }), []);
  const toggleOpen = useCallback(() => dispatch({ _tag: "Toggle" }), []);
  const openAddProject = useCallback(() => dispatch({ _tag: "OpenAddProject" }), []);
  const clearOpenIntent = useCallback(() => dispatch({ _tag: "ClearOpenIntent" }), []);
  // AddProjectDialog must be mounted
  // here, at the always-rendered top level, NOT inside `CommandPaletteDialog`
  // (which unmounts -- returns null -- the instant the palette closes). Its
  // own state closes the palette then opens this dialog; wiring it inside the
  // conditionally-rendered subtree would unmount the dialog before it could
  // ever show.
  const [addProjectDialogOpen, setAddProjectDialogOpen] = useState(false);
  const openAddProjectDialog = useCallback(() => {
    setOpen(false);
    setAddProjectDialogOpen(true);
  }, [setOpen]);
  const keybindings = useAtomValue(primaryServerKeybindingsAtom);
  const composerHandleRef = useRef<ChatComposerHandle | null>(null);
  const routeTarget = useParams({
    strict: false,
    select: (params) => resolveThreadRouteTarget(params),
  });
  const routeThreadRef = routeTarget?.kind === "server" ? routeTarget.threadRef : null;
  const terminalOpen = useThreadHasTerminalSurface(routeThreadRef);

  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const command = resolveShortcutCommand(event, keybindings, {
        context: {
          terminalFocus: isTerminalFocused(),
          terminalOpen,
        },
      });
      if (command !== "commandPalette.toggle") {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      toggleOpen();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [keybindings, terminalOpen, toggleOpen]);

  return (
    <OpenAddProjectCommandPaletteProvider openAddProject={openAddProject}>
      <ComposerHandleContext value={composerHandleRef}>
        <CommandDialog open={state.open} onOpenChange={setOpen}>
          {children}
          <CommandPaletteDialog
            open={state.open}
            openIntent={state.openIntent}
            setOpen={setOpen}
            clearOpenIntent={clearOpenIntent}
            openAddProjectDialog={openAddProjectDialog}
          />
        </CommandDialog>
      </ComposerHandleContext>
      <AddProjectDialog open={addProjectDialogOpen} onOpenChange={setAddProjectDialogOpen} />
    </OpenAddProjectCommandPaletteProvider>
  );
}

function CommandPaletteDialog(props: {
  readonly open: boolean;
  readonly openIntent: CommandPaletteOpenIntent | null;
  readonly setOpen: (open: boolean) => void;
  readonly clearOpenIntent: () => void;
  readonly openAddProjectDialog: () => void;
}) {
  if (!props.open) {
    return null;
  }

  return (
    <OpenCommandPaletteDialog
      openIntent={props.openIntent}
      setOpen={props.setOpen}
      clearOpenIntent={props.clearOpenIntent}
      openAddProjectDialog={props.openAddProjectDialog}
    />
  );
}

function OpenCommandPaletteDialog(props: {
  readonly openIntent: CommandPaletteOpenIntent | null;
  readonly setOpen: (open: boolean) => void;
  readonly clearOpenIntent: () => void;
  readonly openAddProjectDialog: () => void;
}) {
  const navigate = useNavigate();
  const { clearOpenIntent, openIntent, setOpen, openAddProjectDialog } = props;
  const composerHandleRef = useComposerHandleContext();
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const isActionsOnly = deferredQuery.startsWith(">");
  const clientSettings = useClientSettings();
  const { activeDraftThread, activeThread, defaultProjectRef, handleNewThread } =
    useHandleNewThread();
  const projects = useProjects();
  const threads = useThreadShells();
  const { environments } = useEnvironments();
  const activeEnvironmentId = useActiveEnvironmentId();
  const addProjectTargetLabel = resolveAddProjectTargetLabel({
    activeEnvironmentId,
    candidates: environments.map((environment) => ({
      environmentId: environment.environmentId,
      label: environment.label,
      isLocal:
        environment.entry.target._tag === "PrimaryConnectionTarget" ||
        isDesktopLocalConnectionTarget(environment.entry.target),
    })),
  });
  const keybindings = useAtomValue(primaryServerKeybindingsAtom);
  const [viewStack, setViewStack] = useState<CommandPaletteView[]>([]);
  const currentView = viewStack.at(-1) ?? null;
  const paletteMode = getCommandPaletteMode({ currentView, isBrowsing: false });
  const projectTitleById = useMemo(
    () => new Map<ProjectId, string>(projects.map((project) => [project.id, project.title])),
    [projects],
  );

  const activeThreadId = activeThread?.id;
  const deviceAvailability = useDevicePreviewsAvailability(activeThread?.environmentId ?? null);
  const currentProjectId = activeThread?.projectId ?? activeDraftThread?.projectId ?? null;

  const openProjectFromSearch = useMemo(
    () => async (project: (typeof projects)[number]) => {
      const latestThread = getLatestThreadForProject(
        threads.filter((thread) => thread.environmentId === project.environmentId),
        project.id,
        clientSettings.sidebarThreadSortOrder,
      );
      if (latestThread) {
        await navigate({
          to: "/$environmentId/$threadId",
          params: buildThreadRouteParams(
            scopeThreadRef(latestThread.environmentId, latestThread.id),
          ),
        });
        return;
      }

      await handleNewThread(scopeProjectRef(project.environmentId, project.id));
    },
    [handleNewThread, navigate, clientSettings.sidebarThreadSortOrder, threads],
  );

  const projectSearchItems = useMemo(
    () =>
      buildProjectActionItems({
        projects,
        valuePrefix: "project",
        icon: (project) => (
          <ProjectFavicon
            environmentId={project.environmentId}
            cwd={project.workspaceRoot}
            className={ITEM_ICON_CLASS}
          />
        ),
        runProject: openProjectFromSearch,
      }),
    [openProjectFromSearch, projects],
  );

  const projectThreadItems = useMemo(
    () =>
      buildProjectActionItems({
        projects,
        valuePrefix: "new-thread-in",
        shortcutCommand: "chat.new",
        icon: (project) => (
          <ProjectFavicon
            environmentId={project.environmentId}
            cwd={project.workspaceRoot}
            className={ITEM_ICON_CLASS}
          />
        ),
        runProject: async (project) => {
          await startNewThreadInProjectFromContext(
            {
              activeDraftThread,
              activeThread: activeThread ?? undefined,
              defaultProjectRef,
              handleNewThread,
            },
            scopeProjectRef(project.environmentId, project.id),
          );
        },
      }),
    [activeDraftThread, activeThread, defaultProjectRef, handleNewThread, projects],
  );

  const allThreadItems = useMemo(
    () =>
      buildThreadActionItems({
        threads,
        ...(activeThreadId ? { activeThreadId } : {}),
        projectTitleById,
        sortOrder: clientSettings.sidebarThreadSortOrder,
        icon: <MessageSquareIcon className={ITEM_ICON_CLASS} />,
        renderLeadingContent: (thread) => <ThreadRowLeadingStatus thread={thread} />,
        renderTrailingContent: (thread) => <ThreadRowTrailingStatus thread={thread} />,
        runThread: async (thread) => {
          await navigate({
            to: "/$environmentId/$threadId",
            params: buildThreadRouteParams(scopeThreadRef(thread.environmentId, thread.id)),
          });
        },
      }),
    [activeThreadId, clientSettings.sidebarThreadSortOrder, navigate, projectTitleById, threads],
  );
  const recentThreadItems = allThreadItems.slice(0, RECENT_THREAD_LIMIT);

  function pushPaletteView(view: CommandPaletteView): void {
    setViewStack((previousViews) => [
      ...previousViews,
      {
        addonIcon: view.addonIcon,
        groups: view.groups,
        ...(view.initialQuery ? { initialQuery: view.initialQuery } : {}),
      },
    ]);
    setQuery(view.initialQuery ?? "");
  }

  function pushView(item: CommandPaletteSubmenuItem): void {
    pushPaletteView({
      addonIcon: item.addonIcon,
      groups: item.groups,
      ...(item.initialQuery ? { initialQuery: item.initialQuery } : {}),
    });
  }

  function popView(): void {
    setViewStack((previousViews) => previousViews.slice(0, -1));
    setQuery("");
  }

  function handleQueryChange(nextQuery: string): void {
    setQuery(nextQuery);
    if (nextQuery === "" && currentView?.initialQuery) {
      popView();
    }
  }

  const openAddProjectFlow = useCallback(() => {
    openAddProjectDialog();
  }, [openAddProjectDialog]);

  useLayoutEffect(() => {
    if (openIntent?.kind !== "add-project") {
      return;
    }
    clearOpenIntent();
    openAddProjectFlow();
  }, [clearOpenIntent, openAddProjectFlow, openIntent]);

  const actionItems: Array<CommandPaletteActionItem | CommandPaletteSubmenuItem> = [];

  if (projects.length > 0) {
    const activeProjectTitle = currentProjectId
      ? (projectTitleById.get(currentProjectId) ?? null)
      : null;

    if (activeProjectTitle) {
      actionItems.push({
        kind: "action",
        value: "action:new-thread",
        searchTerms: ["new thread", "chat", "create", "draft"],
        title: (
          <>
            New thread in <span className="font-semibold">{activeProjectTitle}</span>
          </>
        ),
        icon: <SquarePenIcon className={ITEM_ICON_CLASS} />,
        shortcutCommand: "chat.new",
        run: async () => {
          await startNewThreadFromContext({
            activeDraftThread,
            activeThread: activeThread ?? undefined,
            defaultProjectRef,
            handleNewThread,
          });
        },
      });
    }

    actionItems.push({
      kind: "submenu",
      value: "action:new-thread-in",
      searchTerms: ["new thread", "project", "pick", "choose", "select"],
      title: "New thread in...",
      icon: <SquarePenIcon className={ITEM_ICON_CLASS} />,
      addonIcon: <SquarePenIcon className={ADDON_ICON_CLASS} />,
      groups: [{ value: "projects", label: "Projects", items: projectThreadItems }],
    });
  }

  actionItems.push({
    kind: "action",
    value: "action:add-project",
    searchTerms: ["add project", "folder", "directory", "browse", "clone", "repository", "git"],
    title:
      addProjectTargetLabel === null ? "Add project" : `Add project on ${addProjectTargetLabel}`,
    icon: <FolderPlusIcon className={ITEM_ICON_CLASS} />,
    keepOpen: true,
    run: async () => {
      openAddProjectDialog();
    },
  });

  if (activeThread && deviceAvailability.enabled) {
    const threadRef = scopeThreadRef(activeThread.environmentId, activeThread.id);
    actionItems.push({
      kind: "action",
      value: "action:open-device-preview",
      searchTerms: ["device", "android", "emulator", "phone", "preview", "mobile"],
      title: "Open device preview",
      icon: <SmartphoneIcon className={ITEM_ICON_CLASS} />,
      run: async () => {
        useRightPanelStore.getState().open(threadRef, "device");
      },
    });
  }

  actionItems.push({
    kind: "action",
    value: "action:settings",
    searchTerms: ["settings", "preferences", "configuration", "keybindings"],
    title: "Open settings",
    icon: <SettingsIcon className={ITEM_ICON_CLASS} />,
    run: async () => {
      await navigate({ to: "/settings" });
    },
  });

  const rootGroups = buildRootGroups({ actionItems, recentThreadItems });
  const activeGroups = currentView?.groups ?? rootGroups;

  const filteredGroups = filterCommandPaletteGroups({
    activeGroups,
    query: deferredQuery,
    isInSubmenu: currentView !== null,
    projectSearchItems: projectSearchItems,
    threadSearchItems: allThreadItems,
  });
  const displayedGroups: CommandPaletteView["groups"] = filteredGroups;
  const inputPlaceholder = getCommandPaletteInputPlaceholder(paletteMode);
  const isSubmenu = paletteMode === "submenu" || paletteMode === "submenu-browse";

  function handleKeyDown(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === "Backspace" && query === "" && isSubmenu) {
      event.preventDefault();
      popView();
    }
  }

  function executeItem(item: CommandPaletteActionItem | CommandPaletteSubmenuItem): void {
    if (item.disabled) {
      return;
    }

    if (item.kind === "submenu") {
      pushView(item);
      return;
    }

    if (!item.keepOpen) {
      setOpen(false);
    }

    void item.run().catch((error: unknown) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Unable to run command",
          description: error instanceof Error ? error.message : "An unexpected error occurred.",
        }),
      );
    });
  }

  return (
    <CommandDialogPopup
      aria-label="Command palette"
      className="overflow-hidden p-0"
      data-command-palette="true"
      data-testid="command-palette"
      finalFocus={() => {
        composerHandleRef?.current?.focusAtEnd();
        return false;
      }}
      onBackdropPointerDown={() => {
        setOpen(false);
      }}
    >
      <Command
        key={viewStack.length}
        aria-label="Command palette"
        autoHighlight="always"
        mode="none"
        onValueChange={handleQueryChange}
        value={query}
      >
        <div className="relative">
          <CommandInput
            placeholder={inputPlaceholder}
            wrapperClassName={
              isSubmenu ? "[&_[data-slot=autocomplete-start-addon]]:pointer-events-auto" : undefined
            }
            {...(isSubmenu
              ? {
                  startAddon: (
                    <button
                      type="button"
                      className="flex cursor-pointer items-center"
                      aria-label="Back"
                      onClick={popView}
                    >
                      <ArrowLeftIcon />
                    </button>
                  ),
                }
              : {})}
            onKeyDown={handleKeyDown}
          />
        </div>
        <CommandPanel className="max-h-[min(28rem,70vh)]">
          <CommandPaletteResults
            groups={displayedGroups}
            isActionsOnly={isActionsOnly}
            keybindings={keybindings}
            onExecuteItem={executeItem}
          />
        </CommandPanel>
        <CommandFooter className="gap-3 max-sm:flex-col max-sm:items-start">
          <div className="flex items-center gap-3">
            <KbdGroup className="items-center gap-1.5">
              <Kbd>
                <ArrowUpIcon />
              </Kbd>
              <Kbd>
                <ArrowDownIcon />
              </Kbd>
              <span className={cn("text-muted-foreground/80")}>Navigate</span>
            </KbdGroup>
            <KbdGroup className="items-center gap-1.5">
              <Kbd>Enter</Kbd>
              <span className={cn("text-muted-foreground/80")}>Select</span>
            </KbdGroup>
            {isSubmenu ? (
              <KbdGroup className="items-center gap-1.5">
                <Kbd>Backspace</Kbd>
                <span className={cn("text-muted-foreground/80")}>Back</span>
              </KbdGroup>
            ) : null}
            <KbdGroup className="items-center gap-1.5">
              <Kbd>Esc</Kbd>
              <span className={cn("text-muted-foreground/80")}>Close</span>
            </KbdGroup>
          </div>
        </CommandFooter>
      </Command>
    </CommandDialogPopup>
  );
}
