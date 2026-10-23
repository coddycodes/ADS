import type {
  EnvironmentId,
  GitManagerChangedFile,
  GitManagerStashEntry,
  ScopedProjectRef,
} from "@bibcode/contracts";
import { GitManagerOperationError } from "@bibcode/contracts";
import { LegendList } from "@legendapp/list/react";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import { AsyncResult } from "effect/reactivity";
import { FileIcon } from "lucide-react";
import { lazy, memo, Suspense, useCallback, useEffect, useMemo, useRef } from "react";

import { DraftId } from "~/composerDraftStore";
import { useTheme } from "~/hooks/useTheme";
import {
  buildFileDiffRenderKey,
  getRenderablePatch,
  resolveDiffThemeName,
  resolveFileDiffPath,
} from "~/lib/diffRendering";
import { cn } from "~/lib/utils";
import { gitManagerEnvironment } from "~/state/gitManager";
import { useEnvironmentQuery } from "~/state/query";

import { AnnotatableCodeView } from "../../diffs/AnnotatableCodeView";

const STASH_FILE_ROW_HEIGHT = 29;
const FILE_LIST_STYLE = Object.freeze({ height: "100%" });
const fixedFileSize = () => STASH_FILE_ROW_HEIGHT;
const fileKey = (file: GitManagerChangedFile) => file.path;
const isGitManagerOperationError = Schema.is(GitManagerOperationError);
const renderHeaderPrefix = () => null;
const DIFF_PREPARING_FALLBACK = (
  <p className="p-3 text-xs text-muted-foreground" role="status">
    Preparing stash diff…
  </p>
);
const DiffWorkerPoolProvider = lazy(async () => {
  const module = await import("../../DiffWorkerPoolProvider");
  return { default: module.DiffWorkerPoolProvider };
});

function isMissingStashFailure(result: AsyncResult.AsyncResult<unknown, unknown>): boolean {
  if (result._tag !== "Failure") return false;
  const failure = Cause.squash(result.cause);
  return isGitManagerOperationError(failure) && failure.code === "stash-not-found";
}

interface StashFileRowProps {
  readonly file: GitManagerChangedFile;
  readonly selected: boolean;
  readonly onSelectPath: (path: string) => void;
}

function stashFileRowPropsEqual(
  previous: Readonly<StashFileRowProps>,
  next: Readonly<StashFileRowProps>,
): boolean {
  return (
    previous.file.path === next.file.path &&
    previous.file.status === next.file.status &&
    previous.file.insertions === next.file.insertions &&
    previous.file.deletions === next.file.deletions &&
    previous.selected === next.selected &&
    previous.onSelectPath === next.onSelectPath
  );
}

const StashFileRow = memo(function StashFileRow({
  file,
  selected,
  onSelectPath,
}: StashFileRowProps) {
  const select = useCallback(() => onSelectPath(file.path), [file.path, onSelectPath]);
  return (
    <button
      aria-selected={selected}
      className={cn(
        "flex h-[29px] w-full min-w-0 items-center gap-1.5 px-2 text-left text-xs focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-ring",
        selected ? "bg-accent text-accent-foreground" : "hover:bg-muted/45",
      )}
      role="option"
      title={file.path}
      translate="no"
      type="button"
      onClick={select}
    >
      <FileIcon aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate">{file.path}</span>
      <span className="shrink-0 text-xs text-muted-foreground">
        +{file.insertions} −{file.deletions}
      </span>
    </button>
  );
}, stashFileRowPropsEqual);

export interface GitManagerStashDiffProps {
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly projectRef: ScopedProjectRef;
  readonly entries: ReadonlyArray<GitManagerStashEntry>;
  readonly stashesPending?: boolean;
  readonly selectedStashSha: string | null;
  readonly selectedPath: string | null;
  readonly onSelectPath: (path: string) => void;
  readonly onRefreshStashes: () => void;
}

export const GitManagerStashDiff = memo(function GitManagerStashDiff({
  scope,
  projectRef,
  entries,
  stashesPending = false,
  selectedStashSha,
  selectedPath,
  onSelectPath,
  onRefreshStashes,
}: GitManagerStashDiffProps) {
  const { environmentId, cwd } = scope;
  const { resolvedTheme } = useTheme();
  const selectedEntry = useMemo(
    () => entries.find((entry) => entry.sha === selectedStashSha) ?? null,
    [entries, selectedStashSha],
  );
  const activePath =
    selectedEntry === null
      ? null
      : selectedEntry.files.some((file) => file.path === selectedPath)
        ? selectedPath
        : (selectedEntry.files[0]?.path ?? null);
  const diffAtom = useMemo(
    () =>
      selectedStashSha === null || selectedEntry === null || activePath === null
        ? null
        : gitManagerEnvironment.getDiff({
            environmentId,
            input: {
              cwd,
              source: { _tag: "stash", sha: selectedStashSha, path: activePath },
            },
          }),
    [activePath, cwd, environmentId, selectedEntry, selectedStashSha],
  );
  const diffQuery = useEnvironmentQuery(diffAtom);
  const missingFromList = selectedStashSha !== null && selectedEntry === null && !stashesPending;
  const missingFromDiff = isMissingStashFailure(diffQuery.emission);
  const missingRefreshKey =
    missingFromList || missingFromDiff
      ? `${selectedStashSha ?? "none"}:${missingFromDiff ? "diff" : "list"}`
      : null;
  const lastMissingRefreshKey = useRef<string | null>(null);
  useEffect(() => {
    if (missingRefreshKey === null) {
      lastMissingRefreshKey.current = null;
      return;
    }
    if (lastMissingRefreshKey.current === missingRefreshKey) return;
    lastMissingRefreshKey.current = missingRefreshKey;
    onRefreshStashes();
  }, [missingRefreshKey, onRefreshStashes]);

  const patch = diffQuery.data?._tag === "patch" ? diffQuery.data.patch : null;
  const renderablePatch = useMemo(
    () => getRenderablePatch(patch ?? undefined, "git-manager-stash"),
    [patch],
  );
  const codeViewFiles = useMemo(
    () =>
      renderablePatch?.kind === "files"
        ? renderablePatch.files.map((fileDiff) => ({
            fileDiff,
            filePath: resolveFileDiffPath(fileDiff),
            fileKey: buildFileDiffRenderKey(fileDiff),
            collapsed: false,
          }))
        : [],
    [renderablePatch],
  );
  const codeViewOptions = useMemo(
    () => ({
      collapsed: false,
      diffStyle: "unified" as const,
      lineDiffType: "none" as const,
      overflow: "scroll" as const,
      theme: resolveDiffThemeName(resolvedTheme),
      themeType: resolvedTheme,
      stickyHeaders: true,
    }),
    [resolvedTheme],
  );
  const composerDraftTarget = useMemo(
    () => DraftId.make(`git-manager-stash:${environmentId}:${projectRef.projectId}`),
    [environmentId, projectRef.projectId],
  );
  const renderFile = useCallback(
    ({ item }: { item: GitManagerChangedFile; index: number }) => (
      <StashFileRow file={item} selected={item.path === activePath} onSelectPath={onSelectPath} />
    ),
    [activePath, onSelectPath],
  );

  if (selectedStashSha === null) {
    return (
      <p className="p-3 text-xs text-muted-foreground">Select a stash to inspect its files.</p>
    );
  }
  if (stashesPending && selectedEntry === null) {
    return (
      <p className="p-3 text-xs text-muted-foreground" role="status">
        Loading stash list…
      </p>
    );
  }
  if (missingFromList || missingFromDiff) {
    return (
      <p aria-live="polite" className="p-3 text-xs text-muted-foreground">
        Stash entry no longer present. Refreshing the repository stash list…
      </p>
    );
  }
  if (selectedEntry === null) return null;

  return (
    <section aria-label={`Stash ${selectedEntry.index} diff`} className="flex min-h-0 flex-1">
      <div
        className="min-h-0 w-[min(18rem,35%)] shrink-0 border-r border-panel-separator"
        role="listbox"
      >
        {selectedEntry.files.length === 0 ? (
          <p className="p-3 text-xs text-muted-foreground">No changed files in this stash.</p>
        ) : (
          <LegendList<GitManagerChangedFile>
            className="size-full min-w-0 overflow-x-hidden overscroll-y-contain"
            data={selectedEntry.files}
            estimatedItemSize={STASH_FILE_ROW_HEIGHT}
            getFixedItemSize={fixedFileSize}
            keyExtractor={fileKey}
            renderItem={renderFile}
            style={FILE_LIST_STYLE}
          />
        )}
      </div>
      <div aria-live="polite" className="min-h-0 min-w-0 flex-1 overflow-auto p-2">
        {activePath === null ? (
          <p className="p-3 text-xs text-muted-foreground">Select a changed file.</p>
        ) : diffQuery.isPending && diffQuery.data === null ? (
          <p className="p-3 text-xs text-muted-foreground" role="status">
            Loading stash diff…
          </p>
        ) : diffQuery.error !== null && diffQuery.data === null ? (
          <p className="p-3 text-xs text-destructive">{diffQuery.error}</p>
        ) : diffQuery.data?._tag === "large-text" ? (
          <p className="p-3 text-xs text-muted-foreground">
            This stash diff is too large to render.
          </p>
        ) : diffQuery.data?._tag === "unrenderable" ? (
          <p className="p-3 text-xs text-muted-foreground">This stash diff cannot be rendered.</p>
        ) : diffQuery.data?._tag === "image" ? (
          <p className="p-3 text-xs text-muted-foreground">
            Image stash diffs are not available yet.
          </p>
        ) : renderablePatch?.kind === "files" ? (
          <Suspense fallback={DIFF_PREPARING_FALLBACK}>
            <DiffWorkerPoolProvider>
              <AnnotatableCodeView
                className="diff-render-surface h-full min-h-0 overflow-auto"
                composerDraftTarget={composerDraftTarget}
                files={codeViewFiles}
                options={codeViewOptions}
                renderHeaderPrefix={renderHeaderPrefix}
                sectionId={`git-manager-stash:${selectedEntry.sha}`}
                sectionTitle={`Stash ${selectedEntry.index}`}
              />
            </DiffWorkerPoolProvider>
          </Suspense>
        ) : renderablePatch?.kind === "raw" ? (
          <div className="space-y-2">
            <p className="text-xs text-muted-foreground">{renderablePatch.reason}</p>
            <pre className="overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/25 p-3 font-mono text-xs">
              {renderablePatch.text}
            </pre>
          </div>
        ) : (
          <p className="p-3 text-xs text-muted-foreground">No diff content.</p>
        )}
      </div>
    </section>
  );
});
