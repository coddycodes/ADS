import { DEFAULT_SERVER_SETTINGS, ProviderInstanceId } from "@bibcode/contracts";
import type {
  EditorId,
  EnvironmentId,
  ProjectEntry,
  ScopedThreadRef,
  VcsStatusResult,
  VcsWorkingTreeFileStatus,
} from "@bibcode/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import type { FileTreeDropResult, GitStatus, GitStatusEntry } from "@pierre/trees";
import { FileTree, useFileTree } from "@pierre/trees/react";
import { FolderClosed, FolderOpen, RefreshCw, Search } from "lucide-react";
import type { MouseEvent } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { isBrowserPreviewFile, openFileInPreview } from "~/browser/openFileInPreview";
import { sanitizeFrontendLogText } from "~/diagnostics/frontendLogCapture";
import { usePreferredEditor } from "~/editorPreferences";
import { useTheme } from "~/hooks/useTheme";
import { inferProjectTitleFromPath } from "~/lib/projectPaths";
import { cn, newProjectId } from "~/lib/utils";
import { BIBCODE_PIERRE_ICONS } from "~/pierre-icons";
import { canPreviewAnyUrlInRuntime } from "~/previewStateStore";
import { useRightPanelStore } from "~/rightPanelStore";
import { assetEnvironment } from "~/state/assets";
import {
  useEnvironment,
  useEnvironmentConnectionState,
  useEnvironmentHttpBaseUrl,
  usePrimaryEnvironmentId,
} from "~/state/environments";
import { previewEnvironment } from "~/state/preview";
import { projectEnvironment } from "~/state/projects";
import { useEnvironmentQuery } from "~/state/query";
import { shellEnvironment } from "~/state/shell";
import { useAtomCommand } from "~/state/use-atom-command";
import { useAtomQueryRunner } from "~/state/use-atom-query-runner";
import { vcsEnvironment } from "~/state/vcs";
import { resolveProviderSessionSelectionForInstance } from "~/providerSessionSelection";

import { RetryButton } from "../ui/retry-button";
import { stackedThreadToast, toastManager } from "../ui/toast";
import FileEntryDialog, { type FileEntryDialogRequest } from "./FileEntryDialog";
import type { FilePathMutationLease, FilePathMutationRequest } from "./filePathMutationLease";
import { isMarkdownPreviewFile } from "./filePreviewMode";
import FileTreeContextMenu, { type FileTreeMenuActions } from "./FileTreeContextMenu";
import {
  buildFileTreeMenuModel,
  entryName,
  type FileTreeEntryKind,
  joinRelativePath,
  joinWorkspacePath,
  parentRelativePath,
  stripTrailingSlash,
} from "./FileTreeContextMenu.logic";
import { useProjectEntriesQuery } from "./projectFilesQueryState";
import { useFileTransfers } from "./useFileTransfers";

interface FileBrowserPanelProps {
  environmentId: EnvironmentId;
  cwd: string;
  projectName: string;
  threadRef: ScopedThreadRef;
  availableEditors: ReadonlyArray<EditorId>;
  onOpenFile: (relativePath: string) => void;
  /**
   * Acquires a per-path editing-session lease before rename/delete/duplicate
   * filesystem operations. Returning null prevents the mutation.
   */
  onBeginPathMutation?: (request: FilePathMutationRequest) => Promise<FilePathMutationLease | null>;
  workspaceUnavailable?: string | null;
}

const TREE_UNSAFE_CSS = `
  :host {
    --trees-bg-override: transparent;
    --trees-selected-bg-override: color-mix(in srgb, currentColor 12%, transparent);
    --trees-hover-bg-override: color-mix(in srgb, currentColor 7%, transparent);
    --trees-border-color-override: color-mix(in srgb, currentColor 14%, transparent);
    --trees-font-family-override: var(--font-sans);
    --trees-font-size-override: 12px;
  }
  button[data-type='item'] { border-radius: 5px; }
`;

/** Stable identity while the query is pending, so the path-reset effect does not run every render. */
const NO_ENTRIES: ReadonlyArray<ProjectEntry> = Object.freeze([]);

function treePath(entry: ProjectEntry): string {
  return entry.kind === "directory" ? `${entry.path}/` : entry.path;
}

const FILE_TREE_GIT_STATUS: Record<VcsWorkingTreeFileStatus, GitStatus> = {
  modified: "modified",
  added: "added",
  deleted: "deleted",
  renamed: "renamed",
  copied: "added",
  untracked: "untracked",
  conflicted: "modified",
};

const FILE_TREE_GIT_STATUS_PRIORITY: Record<GitStatus, number> = {
  ignored: 0,
  renamed: 1,
  added: 2,
  untracked: 2,
  modified: 3,
  deleted: 4,
};

function setDominantGitStatus(statuses: Map<string, GitStatus>, path: string, status: GitStatus) {
  const current = statuses.get(path);
  if (
    current === undefined ||
    FILE_TREE_GIT_STATUS_PRIORITY[status] > FILE_TREE_GIT_STATUS_PRIORITY[current]
  ) {
    statuses.set(path, status);
  }
}

export function buildFileTreeGitStatus(
  entries: ReadonlyArray<ProjectEntry>,
  vcsStatus: VcsStatusResult | null,
): GitStatusEntry[] {
  const statuses = new Map<string, GitStatus>();
  for (const entry of entries) {
    if (entry.ignored) statuses.set(treePath(entry), "ignored");
  }
  for (const file of vcsStatus?.workingTree.files ?? []) {
    const path = file.path.replaceAll("\\", "/");
    const status = FILE_TREE_GIT_STATUS[file.status ?? "modified"];
    setDominantGitStatus(statuses, path, status);
    for (let index = path.lastIndexOf("/"); index > 0; index = path.lastIndexOf("/", index - 1)) {
      setDominantGitStatus(statuses, `${path.slice(0, index)}/`, status);
    }
  }
  return Array.from(statuses, ([path, status]) => ({ path, status }));
}

interface CollapsibleTreeItem {
  collapse?: () => void;
  isDirectory: () => boolean;
  isExpanded?: () => boolean;
}

interface CollapsibleTreeModel {
  getItem: (path: string) => CollapsibleTreeItem | null;
}

export function expandedDirectoryTreePaths(entries: ReadonlyArray<ProjectEntry>): string[] {
  return entries.filter((entry) => entry.kind === "directory").map(treePath);
}

/**
 * The directories the live tree currently shows expanded. `resetPaths` rebuilds the path store and
 * re-applies `initialExpansion` unless it is handed the expansion to keep, so every refresh has to
 * carry the current expansion forward.
 */
export function currentlyExpandedTreePaths(
  model: CollapsibleTreeModel,
  directoryTreePaths: ReadonlyArray<string>,
): string[] {
  return directoryTreePaths.filter((path) => {
    const item = model.getItem(path);
    return item?.isDirectory() === true && item.isExpanded?.() === true;
  });
}

/**
 * The header's second line: rescan or load progress, or the size of the list on screen.
 * Without a loaded list (still loading, or failed) there is no count to show.
 */
function fileListStatus(input: {
  readonly rescanning: boolean;
  readonly pending: boolean;
  readonly fileCount: number | null;
}): string | null {
  if (input.rescanning) return "Refreshing…";
  if (input.fileCount !== null) return `${input.fileCount.toLocaleString()} files`;
  return input.pending ? "Indexing…" : null;
}

export function collapseDirectoryTreePaths(
  model: CollapsibleTreeModel,
  directoryTreePaths: ReadonlyArray<string>,
): void {
  for (const path of directoryTreePaths) {
    const item = model.getItem(path);
    if (item?.isDirectory()) item.collapse?.();
  }
}

export default function FileBrowserPanel({
  environmentId,
  cwd,
  projectName,
  threadRef,
  availableEditors,
  onOpenFile,
  onBeginPathMutation,
  workspaceUnavailable = null,
}: FileBrowserPanelProps) {
  const { resolvedTheme } = useTheme();
  const entriesQuery = useProjectEntriesQuery(environmentId, cwd);
  const entries = entriesQuery.data?.entries ?? NO_ENTRIES;
  const vcsStatusQuery = useEnvironmentQuery(
    cwd.length > 0 ? vcsEnvironment.status({ environmentId, input: { cwd } }) : null,
  );
  // The server sweeps a subscribed workspace for changes made outside the app and drops its cached
  // index before signalling, so re-reading the query here is enough — the signal carries no entries
  // of its own.
  const entryChanges = useEnvironmentQuery(
    cwd.length > 0 ? projectEnvironment.subscribeEntries({ environmentId, input: { cwd } }) : null,
  );
  const gitStatus = useMemo(
    () => buildFileTreeGitStatus(entries, vcsStatusQuery.data),
    [entries, vcsStatusQuery.data],
  );
  const entryKinds = useMemo(
    () => new Map(entries.map((entry) => [entry.path, entry.kind] as const)),
    [entries],
  );
  const entryKindsRef = useRef<ReadonlyMap<string, ProjectEntry["kind"]>>(entryKinds);
  const treePaths = useMemo(() => entries.map(treePath), [entries]);
  const expandedTreePaths = useMemo(() => expandedDirectoryTreePaths(entries), [entries]);
  const previousTreePathsRef = useRef<readonly string[]>([]);

  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const environment = useEnvironment(environmentId);
  // A read cannot reach a disconnected environment; the list reads again once it reconnects.
  const connectionPhase = useEnvironmentConnectionState(environmentId).data?.phase ?? null;
  const waitingForConnection = connectionPhase !== null && connectionPhase !== "connected";
  const isPrimaryEnv = primaryEnvironmentId === environmentId;
  const hasWorkspaceRoot = cwd.length > 0;

  const createProject = useAtomCommand(projectEnvironment.create, { reportFailure: false });
  const createEntry = useAtomCommand(projectEnvironment.createEntry, { reportFailure: false });
  const refreshServerEntries = useAtomCommand(projectEnvironment.refreshEntries, {
    reportFailure: false,
  });
  const renameEntry = useAtomCommand(projectEnvironment.renameEntry, { reportFailure: false });
  const deleteEntry = useAtomCommand(projectEnvironment.deleteEntry, { reportFailure: false });
  const duplicateEntry = useAtomCommand(projectEnvironment.duplicateEntry, {
    reportFailure: false,
  });
  const openInEditor = useAtomCommand(shellEnvironment.openInEditor, "open in editor");

  const environmentHttpBaseUrl = useEnvironmentHttpBaseUrl(environmentId);
  const createAssetUrl = useAtomQueryRunner(assetEnvironment.createUrl, { reportFailure: false });
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: false });
  const [preferredEditor] = usePreferredEditor(availableEditors);

  const remapFileSurfaces = useRightPanelStore((state) => state.remapFileSurfaces);
  const closeFileSurfacesUnder = useRightPanelStore((state) => state.closeFileSurfacesUnder);

  const [isRescanning, setIsRescanning] = useState(false);
  const rescanInFlightRef = useRef(false);
  const [dialogRequest, setDialogRequest] = useState<FileEntryDialogRequest | null>(null);
  const workspaceUnavailableRef = useRef(workspaceUnavailable);
  // Right-click on empty tree space: Pierre only surfaces row menus, so a background menu is
  // opened from the container's contextmenu event, anchored to the cursor point.
  const [backgroundMenu, setBackgroundMenu] = useState<{ x: number; y: number } | null>(null);

  const copyText = useCallback((text: string) => {
    void navigator.clipboard.writeText(text).catch((error: unknown) => {
      console.error(error);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Unable to copy to clipboard",
          description: error instanceof Error ? error.message : "An error occurred.",
        }),
      );
    });
  }, []);

  // The projects.* mutations fail with a structured `failure` code. The outward-symlink refusal is a
  // deliberate safety block (Agent A), so it gets a plain explanation; everything else shows the
  // server message.
  const showMutationError = useCallback((error: unknown, title: string) => {
    const failure =
      typeof error === "object" && error !== null && "failure" in error
        ? (error as { failure?: unknown }).failure
        : undefined;
    const message =
      typeof error === "string"
        ? error
        : typeof error === "object" && error !== null && "message" in error
          ? error.message
          : undefined;
    const description = sanitizeFrontendLogText(
      failure === "resolved_path_outside_root"
        ? "Can't operate on a symlink that points outside the workspace."
        : typeof message === "string" && message.trim()
          ? message
          : "An error occurred.",
    ).replace(/\/api\/transfers\/[^\s"'<>)]*/g, "/api/transfers/[REDACTED]");
    console.error("[file-browser] operation failed", { title, message: description });
    toastManager.add(
      stackedThreadToast({
        type: "error",
        title,
        description,
      }),
    );
  }, []);

  // Download and Upload are the panel's only awaitable, multi-step flows — a minted URL, a
  // transport that differs between desktop and browser, and a prompt the upload waits on — so
  // they live in their own hook. The panel still owns the dialog state the prompt writes into.
  const { downloadEntry, uploadTo, handleUploadInputChange, uploadInputRef, closeDialog } =
    useFileTransfers({
      environmentId,
      cwd,
      httpBaseUrl: environmentHttpBaseUrl,
      workspaceUnavailable,
      showMutationError,
      setDialogRequest,
      refreshEntries: entriesQuery.revalidate,
    });

  useEffect(() => {
    workspaceUnavailableRef.current = workspaceUnavailable;
    if (workspaceUnavailable) {
      closeDialog();
      setBackgroundMenu(null);
    }
  }, [closeDialog, workspaceUnavailable]);

  // The server keeps a cached workspace index, so refreshing the query atom alone replays the stale
  // list. This is the user-initiated rescan: it asks the server to rebuild first, then refreshes the
  // query — including when the rebuild itself fails, so the tree still resyncs with whatever the
  // server has. The projects.* mutations already drop the server's cached index themselves, so they
  // must NOT come through here; a second rebuild request would ship the whole entry list twice.
  const rescanFiles = useCallback(() => {
    if (rescanInFlightRef.current) return;
    rescanInFlightRef.current = true;
    setIsRescanning(true);
    void (async () => {
      try {
        const result = await refreshServerEntries({
          environmentId,
          input: { cwd, refresh: true },
        });
        if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
          showMutationError(squashAtomCommandFailure(result), "Failed to refresh workspace files");
        }
      } catch (error) {
        showMutationError(error, "Failed to refresh workspace files");
      } finally {
        rescanInFlightRef.current = false;
        setIsRescanning(false);
        entriesQuery.refresh();
      }
    })();
  }, [cwd, entriesQuery, environmentId, refreshServerEntries, showMutationError]);

  const addAsProject = useCallback(
    (workspaceRoot: string) => {
      void (async () => {
        const serverConfig = environment?.serverConfig;
        const resolution = resolveProviderSessionSelectionForInstance({
          instanceId: ProviderInstanceId.make("codex"),
          providers: serverConfig?.providers ?? [],
          settings: serverConfig?.settings ?? DEFAULT_SERVER_SETTINGS,
        });
        if (resolution.fallback) {
          console.warn("Provider session default fallback", resolution.fallback);
        }
        const result = await createProject({
          environmentId,
          input: {
            projectId: newProjectId(),
            title: inferProjectTitleFromPath(workspaceRoot),
            workspaceRoot,
            createWorkspaceRootIfMissing: true,
            defaultModelSelection: resolution.modelSelection,
          },
        });
        if (result._tag === "Failure") {
          if (!isAtomCommandInterrupted(result)) {
            showMutationError(
              squashAtomCommandFailure(result),
              `Failed to add project at ${workspaceRoot}`,
            );
          }
        }
      })();
    },
    [createProject, environment?.serverConfig, environmentId, showMutationError],
  );

  const createChildEntry = useCallback(
    (targetDir: string, kind: "file" | "directory") => {
      const isFile = kind === "file";
      setDialogRequest({
        mode: "prompt",
        title: isFile ? "New File" : "New Folder",
        ...(targetDir ? { description: `In ${targetDir}` } : {}),
        label: "Name",
        initialValue: "",
        confirmLabel: "Create",
        onSubmit: (name) => {
          if (workspaceUnavailableRef.current) return;
          void (async () => {
            const relativePath = joinRelativePath(targetDir, name);
            const result = await createEntry({ environmentId, input: { cwd, relativePath, kind } });
            if (result._tag === "Failure") {
              if (!isAtomCommandInterrupted(result)) {
                showMutationError(
                  squashAtomCommandFailure(result),
                  `Failed to create ${isFile ? "file" : "folder"} "${name}"`,
                );
              }
              return;
            }
            entriesQuery.revalidate();
            if (isFile) onOpenFile(result.value.relativePath);
          })();
        },
      });
    },
    [createEntry, cwd, entriesQuery, environmentId, onOpenFile, showMutationError],
  );

  const renameEntryAt = useCallback(
    (relativePath: string) => {
      const currentName = entryName(relativePath);
      setDialogRequest({
        mode: "prompt",
        title: "Rename",
        label: "New name",
        initialValue: currentName,
        confirmLabel: "Rename",
        selectBasename: true,
        onSubmit: (name) => {
          if (workspaceUnavailableRef.current) return;
          if (name === currentName) return;
          const toRelativePath = joinRelativePath(parentRelativePath(relativePath), name);
          void (async () => {
            let lease: FilePathMutationLease | null | undefined;
            try {
              lease = await onBeginPathMutation?.({
                kind: "rename",
                fromRelativePath: relativePath,
                toRelativePath,
              });
              if (onBeginPathMutation && !lease) return;
              const result = await renameEntry({
                environmentId,
                input: { cwd, fromRelativePath: relativePath, toRelativePath },
              });
              if (result._tag === "Failure") {
                if (!isAtomCommandInterrupted(result)) {
                  showMutationError(
                    squashAtomCommandFailure(result),
                    `Failed to rename "${currentName}"`,
                  );
                }
                return;
              }
              lease?.commitRename(result.value.relativePath);
              remapFileSurfaces(threadRef, relativePath, result.value.relativePath);
              entriesQuery.revalidate();
            } catch (error) {
              showMutationError(error, `Failed to rename "${currentName}"`);
            } finally {
              lease?.release();
            }
          })();
        },
      });
    },
    [
      cwd,
      entriesQuery,
      environmentId,
      onBeginPathMutation,
      remapFileSurfaces,
      renameEntry,
      showMutationError,
      threadRef,
    ],
  );

  const deleteEntryAt = useCallback(
    (relativePath: string, kind: FileTreeEntryKind) => {
      const name = entryName(relativePath);
      const isDirectory = kind === "directory";
      setDialogRequest({
        mode: "confirm",
        title: `Delete ${isDirectory ? "folder" : "file"}`,
        description: isDirectory
          ? `Delete "${name}" and everything inside it? This can't be undone.`
          : `Delete "${name}"? This can't be undone.`,
        confirmLabel: "Delete",
        destructive: true,
        onConfirm: () => {
          if (workspaceUnavailableRef.current) return;
          void (async () => {
            let lease: FilePathMutationLease | null | undefined;
            try {
              lease = await onBeginPathMutation?.({ kind: "delete", relativePath });
              if (onBeginPathMutation && !lease) return;
              const result = await deleteEntry({ environmentId, input: { cwd, relativePath } });
              if (result._tag === "Failure") {
                if (!isAtomCommandInterrupted(result)) {
                  showMutationError(squashAtomCommandFailure(result), `Failed to delete "${name}"`);
                }
                return;
              }
              lease?.commitDelete();
              closeFileSurfacesUnder(threadRef, relativePath);
              entriesQuery.revalidate();
            } catch (error) {
              showMutationError(error, `Failed to delete "${name}"`);
            } finally {
              lease?.release();
            }
          })();
        },
      });
    },
    [
      closeFileSurfacesUnder,
      cwd,
      deleteEntry,
      entriesQuery,
      environmentId,
      onBeginPathMutation,
      showMutationError,
      threadRef,
    ],
  );

  const duplicateEntryAt = useCallback(
    (relativePath: string) => {
      void (async () => {
        let lease: FilePathMutationLease | null | undefined;
        try {
          lease = await onBeginPathMutation?.({ kind: "duplicate", relativePath });
          if (onBeginPathMutation && !lease) return;
          const result = await duplicateEntry({ environmentId, input: { cwd, relativePath } });
          if (result._tag === "Failure") {
            if (!isAtomCommandInterrupted(result)) {
              showMutationError(
                squashAtomCommandFailure(result),
                `Failed to duplicate "${entryName(relativePath)}"`,
              );
            }
            return;
          }
          entriesQuery.revalidate();
          onOpenFile(result.value.relativePath);
        } catch (error) {
          showMutationError(error, `Failed to duplicate "${entryName(relativePath)}"`);
        } finally {
          lease?.release();
        }
      })();
    },
    [
      cwd,
      duplicateEntry,
      entriesQuery,
      environmentId,
      onBeginPathMutation,
      onOpenFile,
      showMutationError,
    ],
  );

  const openExternalEditor = useCallback(
    (relativePath: string) => {
      if (!preferredEditor) return;
      void openInEditor({
        environmentId,
        input: { cwd: joinWorkspacePath(cwd, relativePath), editor: preferredEditor },
      });
    },
    [cwd, environmentId, openInEditor, preferredEditor],
  );

  const openInFileManager = useCallback(
    (relativePath: string, kind: FileTreeEntryKind) => {
      const bridge = typeof window === "undefined" ? undefined : window.desktopBridge;
      if (!bridge?.openInFileManager) return;
      void bridge
        .openInFileManager(joinWorkspacePath(cwd, relativePath), kind === "directory")
        .catch((error) => showMutationError(error, "Unable to open in file manager"));
    },
    [cwd, showMutationError],
  );

  const openPreviewFor = useCallback(
    (relativePath: string) => {
      if (!environmentHttpBaseUrl) return;
      void (async () => {
        const result = await openFileInPreview({
          threadRef,
          filePath: joinWorkspacePath(cwd, relativePath),
          httpBaseUrl: environmentHttpBaseUrl,
          createAssetUrl,
          openPreview,
        });
        if (result._tag === "Success" || isAtomCommandInterrupted(result)) return;
        showMutationError(squashAtomCommandFailure(result), "Unable to open file in browser");
      })();
    },
    [createAssetUrl, cwd, environmentHttpBaseUrl, openPreview, showMutationError, threadRef],
  );

  // An in-tree drop is a move, which the server exposes as a rename from → to, so it runs through
  // the same editing-session lease and surface remapping as the Rename command. Pierre has already
  // applied the move to its own model by the time it calls back, so every outcome ends in a refresh
  // to resync the tree with the server.
  const moveDroppedEntries = useCallback(
    (event: FileTreeDropResult) => {
      if (workspaceUnavailableRef.current) {
        entriesQuery.revalidate();
        return;
      }
      const targetDir =
        event.target.kind === "root" ? "" : stripTrailingSlash(event.target.directoryPath ?? "");
      void (async () => {
        for (const draggedPath of event.draggedPaths) {
          const fromRelativePath = stripTrailingSlash(draggedPath);
          if (parentRelativePath(fromRelativePath) === targetDir) continue;
          const name = entryName(fromRelativePath);
          const toRelativePath = joinRelativePath(targetDir, name);
          let lease: FilePathMutationLease | null | undefined;
          try {
            lease = await onBeginPathMutation?.({
              kind: "rename",
              fromRelativePath,
              toRelativePath,
            });
            if (onBeginPathMutation && !lease) continue;
            const result = await renameEntry({
              environmentId,
              input: { cwd, fromRelativePath, toRelativePath },
            });
            if (result._tag === "Failure") {
              if (!isAtomCommandInterrupted(result)) {
                showMutationError(squashAtomCommandFailure(result), `Failed to move "${name}"`);
              }
              continue;
            }
            lease?.commitRename(result.value.relativePath);
            remapFileSurfaces(threadRef, fromRelativePath, result.value.relativePath);
          } catch (error) {
            showMutationError(error, `Failed to move "${name}"`);
          } finally {
            lease?.release();
          }
        }
        entriesQuery.revalidate();
      })();
    },
    [
      cwd,
      entriesQuery,
      environmentId,
      onBeginPathMutation,
      remapFileSurfaces,
      renameEntry,
      showMutationError,
      threadRef,
    ],
  );

  // Pierre rejects some drops inside its own model (a name collision, or a directory dropped into
  // its own descendant), in which case onDropComplete never runs. Nothing reached the server, so
  // re-reading its unchanged list is what puts the tree back.
  const reportDropError = useCallback(
    (error: string) => {
      showMutationError(new Error(error), "Failed to move files");
      entriesQuery.revalidate();
    },
    [entriesQuery, showMutationError],
  );

  // useFileTree captures its options once, so the drop callbacks are reached through refs that
  // always hold the current render's handlers.
  const moveDroppedEntriesRef = useRef(moveDroppedEntries);
  const reportDropErrorRef = useRef(reportDropError);
  useEffect(() => {
    moveDroppedEntriesRef.current = moveDroppedEntries;
    reportDropErrorRef.current = reportDropError;
  }, [moveDroppedEntries, reportDropError]);

  const { model } = useFileTree({
    composition: { contextMenu: { enabled: true, triggerMode: "both" } },
    density: "compact",
    dragAndDrop: {
      canDrag: () => !workspaceUnavailableRef.current,
      onDropComplete: (event) => moveDroppedEntriesRef.current(event),
      onDropError: (error) => reportDropErrorRef.current(error),
    },
    fileTreeSearchMode: "hide-non-matches",
    flattenEmptyDirectories: false,
    gitStatus,
    initialExpansion: 0,
    icons: BIBCODE_PIERRE_ICONS,
    onSelectionChange: (selectedPaths) => {
      const selectedPath = selectedPaths.at(-1)?.replace(/\/$/, "");
      if (selectedPath && entryKindsRef.current.get(selectedPath) === "file") {
        onOpenFile(selectedPath);
      }
    },
    paths: [],
    search: true,
    unsafeCSS: TREE_UNSAFE_CSS,
  });

  useEffect(() => {
    if (previousTreePathsRef.current === treePaths) return;
    entryKindsRef.current = entryKinds;
    previousTreePathsRef.current = treePaths;
    model.resetPaths(treePaths, {
      initialExpandedPaths: currentlyExpandedTreePaths(model, expandedTreePaths),
    });
  }, [entryKinds, expandedTreePaths, model, treePaths]);

  useEffect(() => model.setGitStatus(gitStatus), [gitStatus, model]);

  const entryChangeSignal = entryChanges.data;
  // Tracks the signal already acted on. The server starts each subscription with a resync signal
  // after establishing the watcher, so the first value must refresh a list that may have raced it.
  //
  // Recording it is what makes the effect idempotent. `entriesQuery` is a fresh object on every
  // render, so an effect depending on it re-runs constantly; without this the refresh it triggers
  // re-renders, re-runs the effect, and refreshes again in a hot loop. Depend on the stable
  // `revalidate` callback rather than the wrapper object for the same reason; a signal is an
  // automatic read, so it keeps a transport cut-off latched until the user rescans.
  const handledEntryChangeRef = useRef<typeof entryChangeSignal>(null);
  const revalidateEntries = entriesQuery.revalidate;
  useEffect(() => {
    if (entryChangeSignal === null || entryChangeSignal === handledEntryChangeRef.current) {
      return;
    }
    handledEntryChangeRef.current = entryChangeSignal;
    revalidateEntries();
  }, [entryChangeSignal, revalidateEntries]);

  const fileCount = useMemo(
    () => entries.reduce((count, entry) => count + (entry.kind === "file" ? 1 : 0), 0),
    [entries],
  );
  const listStatus = fileListStatus({
    rescanning: isRescanning,
    pending: entriesQuery.isPending,
    fileCount: entriesQuery.data === null ? null : fileCount,
  });

  // The menu model decides which actions show for a file vs a directory, so the full handler set is
  // supplied for every row. New File/Folder target the clicked folder itself, or a file's parent
  // directory. Copy Path uses the workspace-root separator style.
  const rowActions = useCallback(
    (relativePath: string, kind: FileTreeEntryKind): FileTreeMenuActions => {
      const targetDir = kind === "directory" ? relativePath : parentRelativePath(relativePath);
      return {
        onCopyPath: () => copyText(joinWorkspacePath(cwd, relativePath)),
        onCopyRelativePath: () => copyText(relativePath),
        // Download reads, so it stays available even when the workspace is gone from under us.
        onDownload: () => downloadEntry(relativePath),
        ...(workspaceUnavailable === null
          ? {
              onNewFile: () => createChildEntry(targetDir, "file"),
              onNewFolder: () => createChildEntry(targetDir, "directory"),
              ...(typeof window !== "undefined" && window.desktopBridge?.openInFileManager
                ? { onOpenFileManager: () => openInFileManager(relativePath, kind) }
                : {}),
              onDuplicate: () => duplicateEntryAt(relativePath),
              onAddAsProject: () => addAsProject(joinWorkspacePath(cwd, relativePath)),
              onOpenExternalEditor: () => openExternalEditor(relativePath),
              onOpenPreview: () => openPreviewFor(relativePath),
              // The menu model disables Upload on file rows, so this is only reachable from a
              // folder row; it targets a file's parent directory if that ever changes.
              onUpload: () => uploadTo(targetDir),
              onRename: () => renameEntryAt(relativePath),
              onDelete: () => deleteEntryAt(relativePath, kind),
            }
          : {}),
      };
    },
    [
      addAsProject,
      copyText,
      createChildEntry,
      cwd,
      deleteEntryAt,
      downloadEntry,
      duplicateEntryAt,
      openExternalEditor,
      openInFileManager,
      openPreviewFor,
      renameEntryAt,
      uploadTo,
      workspaceUnavailable,
    ],
  );

  const handleBackgroundContextMenu = useCallback((event: MouseEvent<HTMLElement>) => {
    // A row right-click is handled by Pierre, which prevents the default contextmenu event; only
    // unhandled (empty-space) right-clicks reach here.
    if (event.defaultPrevented) return;
    event.preventDefault();
    setBackgroundMenu({ x: event.clientX, y: event.clientY });
  }, []);

  const backgroundAnchor = useMemo(
    () =>
      backgroundMenu
        ? { getBoundingClientRect: () => new DOMRect(backgroundMenu.x, backgroundMenu.y, 0, 0) }
        : null,
    [backgroundMenu],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col bg-background"
      data-file-browser-panel={`${environmentId}:${cwd}`}
    >
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-panel-separator px-3">
        <div className="min-w-0 flex-1">
          <div className="truncate text-xs font-medium text-foreground">{projectName}</div>
          {listStatus === null ? null : (
            <div className="truncate text-xs text-muted-foreground">
              {listStatus}
              {entriesQuery.data?.truncated ? " · partial" : ""}
            </div>
          )}
        </div>
        <button
          type="button"
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          aria-label="Collapse all folders"
          onClick={() => collapseDirectoryTreePaths(model, expandedTreePaths)}
        >
          <FolderClosed className="size-3.5" />
        </button>
        <button
          type="button"
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          aria-label="Expand all folders"
          onClick={() => model.resetPaths(treePaths, { initialExpandedPaths: expandedTreePaths })}
        >
          <FolderOpen className="size-3.5" />
        </button>
        <button
          type="button"
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          aria-label="Search workspace files"
          onClick={() => model.openSearch()}
        >
          <Search className="size-3.5" />
        </button>
        <button
          type="button"
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          aria-label="Refresh workspace files"
          aria-busy={isRescanning}
          disabled={isRescanning}
          onClick={rescanFiles}
        >
          <RefreshCw
            className={cn("size-3.5", (entriesQuery.isPending || isRescanning) && "animate-spin")}
          />
        </button>
      </div>
      {entriesQuery.error && entriesQuery.data === null ? (
        <div className="space-y-2 p-4">
          <p className="text-xs leading-relaxed text-destructive">{entriesQuery.error}</p>
          <RetryButton
            retrying={entriesQuery.isPending}
            waitingForConnection={waitingForConnection}
            onRetry={entriesQuery.refresh}
          />
        </div>
      ) : (
        <FileTree
          model={model}
          aria-label={`${projectName} files`}
          className="min-h-0 flex-1 overflow-hidden"
          onContextMenu={handleBackgroundContextMenu}
          renderContextMenu={(item, context) => {
            const relativePath = stripTrailingSlash(item.path);
            const entryKind: FileTreeEntryKind = item.kind === "directory" ? "directory" : "file";
            const menuModel = buildFileTreeMenuModel({
              entryKind,
              isPreviewable:
                item.kind === "file" &&
                canPreviewAnyUrlInRuntime() &&
                isBrowserPreviewFile(relativePath),
              isMarkdown: item.kind === "file" && isMarkdownPreviewFile(relativePath),
              isPrimaryEnv,
              hasWorkspaceRoot,
            });
            return (
              <FileTreeContextMenu
                model={menuModel}
                actions={rowActions(relativePath, entryKind)}
                anchor={{
                  getBoundingClientRect: () => context.anchorElement.getBoundingClientRect(),
                }}
                onClose={() => context.close()}
              />
            );
          }}
          style={{
            colorScheme: resolvedTheme,
            ["--trees-fg-override" as string]: "var(--foreground)",
            ["--trees-git-added-color-override" as string]: "var(--success)",
            ["--trees-git-deleted-color-override" as string]: "var(--destructive)",
            ["--trees-git-ignored-color-override" as string]: "var(--warning)",
            ["--trees-git-modified-color-override" as string]: "var(--warning)",
            ["--trees-git-renamed-color-override" as string]: "var(--warning)",
            ["--trees-git-untracked-color-override" as string]: "var(--success)",
          }}
        />
      )}
      {backgroundMenu ? (
        <FileTreeContextMenu
          model={buildFileTreeMenuModel({
            entryKind: "background",
            isPreviewable: false,
            isMarkdown: false,
            isPrimaryEnv,
            hasWorkspaceRoot,
          })}
          actions={{
            ...(workspaceUnavailable === null
              ? {
                  onNewFile: () => createChildEntry("", "file"),
                  onNewFolder: () => createChildEntry("", "directory"),
                  onUpload: () => uploadTo(""),
                }
              : {}),
            onCopyPath: () => copyText(cwd),
            onRefresh: () => rescanFiles(),
          }}
          anchor={backgroundAnchor}
          onClose={() => setBackgroundMenu(null)}
        />
      ) : null}
      <input
        ref={uploadInputRef}
        type="file"
        multiple
        hidden
        aria-hidden="true"
        tabIndex={-1}
        onChange={handleUploadInputChange}
      />
      <FileEntryDialog request={dialogRequest} onClose={closeDialog} />
    </div>
  );
}
