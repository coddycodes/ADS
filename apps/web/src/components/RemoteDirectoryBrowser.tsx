import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import type { EnvironmentId } from "@bibcode/contracts";
import { joinHostPath } from "@bibcode/shared/path";
import {
  ArrowUpIcon,
  ChevronRightIcon,
  FolderIcon,
  FolderPlusIcon,
  HardDriveIcon,
  LoaderCircleIcon,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { Button } from "./ui/button";
import { DraftInput } from "./ui/draft-input";
import { Input } from "./ui/input";
import { filesystemEnvironment } from "~/state/filesystem";
import { projectEnvironment } from "~/state/projects";
import { useEnvironmentQuery } from "~/state/query";
import { useAtomCommand } from "~/state/use-atom-command";

export interface RemoteDirectoryBrowserProps {
  readonly environmentId: EnvironmentId;
  readonly initialPath: string;
  /**
   * Resets the browser (path, hidden toggle, warnings, pending create) when this
   * value changes. Pass a value that stays stable while the component is visible — for
   * example the browsing target's `environmentId` — not one that flips during a host
   * dialog's exit transition, since the browser typically stays mounted through it.
   */
  readonly resetKey: unknown;
  /** Called with the canonical directory path when the user confirms. */
  readonly onSelect: (path: string) => void;
  /** Called when the user cancels; omit to hide the Cancel button when the host provides its own way back. */
  readonly onCancel?: () => void;
  /** Optional extra secondary action rendered next to Cancel (Add Project uses "Type a path instead"). */
  readonly secondaryAction?: { readonly label: string; readonly onClick: () => void };
  readonly selectLabel?: string;
}

function validateNewFolderName(name: string): string | null {
  if (name.length === 0) return "Enter a folder name.";
  if (name === "." || name === "..") return "Choose a folder name other than . or ..";
  if (name.includes("/") || name.includes("\\")) {
    return "Use a single folder name without slashes.";
  }
  return null;
}

function createFolderErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim().length > 0) return error.message;
  if (typeof error === "string" && error.trim().length > 0) return error;
  return "Unable to create folder.";
}

/**
 * What the browser is showing. Every change to the target or the path starts a new
 * generation, and work started under an older generation must not land on it.
 */
interface PickerIdentity {
  readonly environmentId: EnvironmentId;
  readonly resetKey: unknown;
  readonly initialPath: string;
  readonly createEntry: unknown;
  readonly path: string;
  readonly generation: number;
}

type CreateFolderOutcome =
  | { readonly kind: "created"; readonly path: string }
  /** `message` is null for an interrupted request, which needs no error. */
  | { readonly kind: "failed"; readonly message: string | null };

interface CreateFolderCompletion {
  /** The picker generation the request started under. */
  readonly generation: number;
  readonly outcome: CreateFolderOutcome;
}

export function RemoteDirectoryBrowser({
  environmentId,
  initialPath,
  resetKey,
  onSelect,
  onCancel,
  secondaryAction,
  selectLabel,
}: RemoteDirectoryBrowserProps) {
  const createEntry = useAtomCommand(projectEnvironment.createEntry, { reportFailure: false });
  const [picker, setPicker] = useState<PickerIdentity>(() => ({
    environmentId,
    resetKey,
    initialPath,
    createEntry,
    path: initialPath || "~",
    generation: 0,
  }));
  const [fallbackWarning, setFallbackWarning] = useState<string | null>(null);
  const [showHiddenFolders, setShowHiddenFolders] = useState(false);
  const [newFolderName, setNewFolderName] = useState<string | null>(null);
  const [createPending, setCreatePending] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  // A completion is judged while rendering, against the picker it would land on, so one that
  // resolves while a replacement renders is rejected by that replacement.
  const [createCompletion, setCreateCompletion] = useState<CreateFolderCompletion | null>(null);
  /** The generation of a created folder whose listing is revalidated once it is shown. */
  const [refreshGeneration, setRefreshGeneration] = useState<number | null>(null);
  const refreshedGenerationRef = useRef<number | null>(null);

  const pickerTargetChanged =
    picker.environmentId !== environmentId ||
    picker.resetKey !== resetKey ||
    picker.initialPath !== initialPath ||
    picker.createEntry !== createEntry;
  if (pickerTargetChanged) {
    setPicker({
      environmentId,
      resetKey,
      initialPath,
      createEntry,
      path: initialPath || "~",
      generation: picker.generation + 1,
    });
    setNewFolderName(null);
    setCreatePending(false);
    setCreateError(null);
    setFallbackWarning(null);
    setShowHiddenFolders(false);
  }
  const path = picker.path;

  const query = useEnvironmentQuery(
    filesystemEnvironment.browse({
      environmentId,
      input: { partialPath: path, mode: "directory" },
    }),
  );
  const directoryPath = query.data?.directoryPath ?? null;
  const breadcrumbs = query.data?.breadcrumbs ?? [];
  const entries = query.data?.entries ?? [];
  const hiddenFolderCount = entries.filter((entry) => entry.name.startsWith(".")).length;
  const visibleEntries = showHiddenFolders
    ? entries
    : entries.filter((entry) => !entry.name.startsWith("."));
  const displayPath = directoryPath ?? path;
  const trimmedFolderName = newFolderName?.trim() ?? "";
  const folderNameError = validateNewFolderName(trimmedFolderName);
  const visibleCreateError =
    trimmedFolderName.length > 0 ? (folderNameError ?? createError) : createError;
  const canCreateInCurrentDirectory =
    directoryPath !== null && !query.isPending && query.error === null;

  // After a reset, wait for the re-render so the checks below see the new picker.
  if (!pickerTargetChanged) {
    if (createCompletion !== null) {
      setCreateCompletion(null);
      if (createCompletion.generation === picker.generation && createPending) {
        setCreatePending(false);
        const { outcome } = createCompletion;
        if (outcome.kind === "failed") {
          if (outcome.message !== null) setCreateError(outcome.message);
        } else {
          const generation = picker.generation + 1;
          setPicker({ ...picker, path: outcome.path, generation });
          setNewFolderName(null);
          setCreateError(null);
          setRefreshGeneration(generation);
        }
      }
    } else if (newFolderName === null && query.error && path !== "~" && fallbackWarning === null) {
      setFallbackWarning("The previous folder is unavailable. Showing the server home directory.");
      setPicker({ ...picker, path: "~", generation: picker.generation + 1 });
      setCreatePending(false);
      setCreateError(null);
    }
  }

  const navigateTo = useCallback((nextPath: string) => {
    setPicker((current) => ({ ...current, path: nextPath, generation: current.generation + 1 }));
    setNewFolderName(null);
    setCreatePending(false);
    setCreateError(null);
  }, []);

  const revalidateQuery = query.revalidate;
  const pickerGeneration = picker.generation;
  useEffect(() => {
    if (
      refreshGeneration === null ||
      refreshGeneration !== pickerGeneration ||
      refreshedGenerationRef.current === refreshGeneration
    ) {
      return;
    }
    refreshedGenerationRef.current = refreshGeneration;
    revalidateQuery();
  }, [pickerGeneration, refreshGeneration, revalidateQuery]);

  const closeNewFolder = () => {
    if (createPending) return;
    setNewFolderName(null);
    setCreateError(null);
  };

  const submitNewFolder = () => {
    if (createPending || !canCreateInCurrentDirectory || directoryPath === null) {
      return;
    }
    if (folderNameError !== null) {
      setCreateError(folderNameError);
      return;
    }

    const { generation } = picker;
    const parentPath = directoryPath;
    setCreatePending(true);
    setCreateError(null);

    const complete = (outcome: CreateFolderOutcome) =>
      // Only one request runs per generation, so the newest generation is the only one that
      // can still be current.
      setCreateCompletion((current) =>
        current !== null && current.generation > generation ? current : { generation, outcome },
      );

    void createEntry({
      environmentId,
      input: { cwd: parentPath, relativePath: trimmedFolderName, kind: "directory" },
    }).then(
      (result) => {
        if (result._tag === "Failure") {
          complete({
            kind: "failed",
            message: isAtomCommandInterrupted(result)
              ? null
              : createFolderErrorMessage(squashAtomCommandFailure(result)),
          });
          return;
        }

        const normalizedParentPath = joinHostPath(parentPath, "");
        const createdPath = joinHostPath(parentPath, result.value.relativePath);
        if (result.value.relativePath.trim().length === 0 || createdPath === normalizedParentPath) {
          complete({
            kind: "failed",
            message: "The server did not return a valid created folder path.",
          });
          return;
        }
        complete({ kind: "created", path: createdPath });
      },
      (error: unknown) => complete({ kind: "failed", message: createFolderErrorMessage(error) }),
    );
  };

  return (
    <div className="space-y-2.5">
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          aria-label="Up one directory"
          disabled={!query.data?.ancestorPath || query.isPending}
          onClick={() => {
            if (query.data?.ancestorPath) navigateTo(query.data.ancestorPath);
          }}
        >
          <ArrowUpIcon className="size-4" />
        </Button>
        <DraftInput
          value={displayPath}
          onCommit={navigateTo}
          aria-label="Server directory path"
          spellCheck={false}
          className="min-w-0 flex-1 font-mono text-xs"
        />
        <Button type="button" variant="outline" size="sm" onClick={query.refresh}>
          Refresh
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label="New folder"
          disabled={!canCreateInCurrentDirectory || createPending}
          onClick={() => {
            setNewFolderName("");
            setCreateError(null);
          }}
        >
          <FolderPlusIcon className="size-4" />
          New folder
        </Button>
      </div>
      {newFolderName !== null ? (
        <form
          className="flex items-start gap-2 rounded-lg border bg-muted/24 p-2"
          onSubmit={(event) => {
            event.preventDefault();
            submitNewFolder();
          }}
        >
          <div className="min-w-0 flex-1 space-y-1">
            <Input
              autoFocus
              aria-label="New folder name"
              value={newFolderName}
              disabled={createPending}
              onChange={(event) => {
                setNewFolderName(event.currentTarget.value);
                setCreateError(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  closeNewFolder();
                } else if (event.key === "Enter") {
                  event.preventDefault();
                  submitNewFolder();
                }
              }}
            />
            {visibleCreateError ? (
              <p role="alert" className="text-xs text-destructive">
                {visibleCreateError}
              </p>
            ) : null}
          </div>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={createPending}
            onClick={closeNewFolder}
          >
            Cancel
          </Button>
          <Button
            type="submit"
            size="sm"
            disabled={folderNameError !== null || createPending || !canCreateInCurrentDirectory}
          >
            {createPending ? "Creating…" : "Create"}
          </Button>
        </form>
      ) : null}
      <nav
        aria-label="Directory breadcrumbs"
        className="flex min-h-8 items-center overflow-x-auto rounded-lg border bg-muted/24 px-1.5"
      >
        {breadcrumbs.map((breadcrumb, index) => {
          const isCurrent = index === breadcrumbs.length - 1;
          return (
            <div key={breadcrumb.fullPath} className="flex shrink-0 items-center">
              {index > 0 ? (
                <ChevronRightIcon
                  aria-hidden="true"
                  className="size-3 text-muted-foreground/50"
                  data-directory-breadcrumb-separator=""
                />
              ) : null}
              <Button
                type="button"
                variant="ghost"
                size="xs"
                aria-label={`Open ${breadcrumb.name}`}
                aria-current={isCurrent ? "page" : undefined}
                className={
                  isCurrent
                    ? "max-w-44 gap-1.5 px-1.5 font-medium"
                    : "max-w-44 gap-1.5 px-1.5 font-normal text-muted-foreground"
                }
                title={breadcrumb.fullPath}
                onClick={() => navigateTo(breadcrumb.fullPath)}
              >
                {index === 0 ? (
                  <HardDriveIcon className="size-3.5" />
                ) : isCurrent ? (
                  <FolderIcon className="size-3.5" />
                ) : null}
                <span className="truncate">{breadcrumb.name}</span>
              </Button>
            </div>
          );
        })}
      </nav>
      <div className="overflow-hidden rounded-lg border bg-background">
        <div className="flex min-h-8 items-center justify-between border-b bg-muted/36 px-3">
          <span className="text-xs font-medium text-muted-foreground">Folders</span>
          {hiddenFolderCount > 0 ? (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              className="-mr-1 h-6 px-1.5 font-normal text-muted-foreground"
              onClick={() => setShowHiddenFolders((current) => !current)}
            >
              {showHiddenFolders ? "Hide hidden folders" : "Show hidden folders"}
            </Button>
          ) : null}
        </div>
        <div className="max-h-72 min-h-48 overflow-y-auto py-1">
          {visibleEntries.map((entry) => (
            <Button
              type="button"
              variant="ghost"
              key={entry.fullPath}
              aria-label={`Open ${entry.name}`}
              className="h-8 w-full justify-start rounded-none border-0 px-3 py-1.5 font-normal text-left shadow-none hover:bg-muted/72"
              onClick={() => navigateTo(entry.fullPath)}
            >
              <FolderIcon
                aria-hidden="true"
                className="size-4 text-muted-foreground/80"
                data-directory-folder-icon=""
              />
              <span className="truncate">{entry.name}</span>
            </Button>
          ))}
          {query.isPending ? (
            <div
              role="status"
              className="flex min-h-40 flex-col items-center justify-center gap-2 px-3 text-sm text-muted-foreground"
            >
              <LoaderCircleIcon className="size-4 animate-spin" />
              <span>Loading folders…</span>
            </div>
          ) : null}
          {!query.isPending && !query.error && visibleEntries.length === 0 ? (
            <div className="flex min-h-40 items-center justify-center px-6 text-center text-sm text-muted-foreground">
              {hiddenFolderCount > 0 && !showHiddenFolders
                ? "No visible folders. Show hidden folders to browse hidden directories."
                : "No folders in this location."}
            </div>
          ) : null}
        </div>
      </div>
      {fallbackWarning ? (
        <p role="status" className="text-sm text-muted-foreground">
          {fallbackWarning}
        </p>
      ) : null}
      {query.error ? (
        <p role="alert" className="text-sm text-destructive">
          {query.error}
        </p>
      ) : null}
      <div className="flex items-center justify-end gap-2 pt-2">
        {onCancel ? (
          <Button type="button" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
        {secondaryAction ? (
          <Button type="button" variant="ghost" onClick={secondaryAction.onClick}>
            {secondaryAction.label}
          </Button>
        ) : null}
        <Button
          type="button"
          disabled={directoryPath === null || query.isPending || query.error !== null}
          onClick={() => {
            if (directoryPath) onSelect(directoryPath);
          }}
        >
          {selectLabel ?? "Select folder"}
        </Button>
      </div>
    </div>
  );
}
