import type { EnvironmentId, GitManagerCommitEntry } from "@bibcode/contracts";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import { FileDiff } from "@pierre/diffs/react";
import { CheckIcon, CopyIcon, FileIcon } from "lucide-react";
import {
  lazy,
  memo,
  Suspense,
  useCallback,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";

import { useTheme } from "../../../hooks/useTheme";
import {
  buildFileDiffRenderKey,
  getRenderablePatch,
  resolveDiffThemeName,
} from "../../../lib/diffRendering";
import { cn } from "../../../lib/utils";
import { gitManagerEnvironment } from "../../../state/gitManager";
import { useEnvironmentQuery } from "../../../state/query";
import { GitManagerStoredImageDiff } from "../diff/GitManagerImageDiffModeContext";
import { buildImageDataUri } from "../diff/gitManagerImageDiff.logic";
import { deriveAuthorIdentity } from "./authorIdentity";
import { classifyDiffPayload } from "./diffLadder";

const DiffWorkerPoolProvider = lazy(async () => {
  const module = await import("../../DiffWorkerPoolProvider");
  return { default: module.DiffWorkerPoolProvider };
});

const identityDateFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "short",
});
const CHANGED_FILE_ROW_HEIGHT = 29;
const getChangedFileRowSize = () => CHANGED_FILE_ROW_HEIGHT;
const getChangedFileKey = (path: string) => path;
const DIFF_PREPARING_FALLBACK = (
  <p role="status" className="p-3 text-xs text-muted-foreground">
    Preparing diff…
  </p>
);

export interface GitManagerCommitDetailProps {
  readonly environmentId: EnvironmentId;
  readonly cwd: string;
  readonly commit: GitManagerCommitEntry;
  readonly selectedFilePath: string | null;
  readonly onSelectFile: (path: string) => void;
}

interface IdentityBadgeProps {
  readonly label: "Author" | "Committer";
  readonly name: string;
  readonly email: string;
  readonly timestampMs: number;
}

const IdentityBadge = memo(function IdentityBadge({
  label,
  name,
  email,
  timestampMs,
}: IdentityBadgeProps) {
  const identity = deriveAuthorIdentity({ name, email });
  return (
    <div className="flex min-w-0 items-center gap-2">
      <span
        aria-hidden="true"
        className="flex size-7 shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white"
        style={{ backgroundColor: `hsl(${identity.hue} 55% 42%)` }}
      >
        {identity.initials}
      </span>
      <span className="min-w-0">
        <span className="block text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {label}
        </span>
        <span className="block truncate text-xs" title={identity.title}>
          {identity.title}
        </span>
        <time
          className="block text-xs text-muted-foreground"
          dateTime={new Date(timestampMs).toISOString()}
        >
          {identityDateFormatter.format(timestampMs)}
        </time>
      </span>
    </div>
  );
});

interface ChangedFileRowProps {
  readonly path: string;
  readonly selected: boolean;
  readonly tabbable: boolean;
  readonly onSelect: (path: string) => void;
}

function changedFileRowPropsEqual(previous: ChangedFileRowProps, next: ChangedFileRowProps) {
  return (
    previous.path === next.path &&
    previous.selected === next.selected &&
    previous.tabbable === next.tabbable &&
    previous.onSelect === next.onSelect
  );
}

const ChangedFileRow = memo(function ChangedFileRow({
  path,
  selected,
  tabbable,
  onSelect,
}: ChangedFileRowProps) {
  return (
    <button
      type="button"
      aria-selected={selected}
      className={cn(
        "flex h-[29px] w-full min-w-0 items-center gap-1.5 px-2 text-left text-xs focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-ring",
        selected ? "bg-accent text-accent-foreground" : "hover:bg-muted/45",
      )}
      data-changed-file-path={path}
      role="option"
      tabIndex={tabbable ? 0 : -1}
      title={path}
      translate="no"
      onClick={() => onSelect(path)}
    >
      <FileIcon aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" />
      <span className="truncate">{path}</span>
    </button>
  );
}, changedFileRowPropsEqual);

interface ChangedFileListProps {
  readonly paths: ReadonlyArray<string>;
  readonly selectedPath: string | null;
  readonly onSelect: (path: string) => void;
}

const ChangedFileList = memo(function ChangedFileList({
  paths,
  selectedPath,
  onSelect,
}: ChangedFileListProps) {
  const listRef = useRef<LegendListRef | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const renderFile = useCallback(
    ({ item, index }: { item: string; index: number }) => (
      <ChangedFileRow
        path={item}
        selected={item === selectedPath}
        tabbable={item === selectedPath || (selectedPath === null && index === 0)}
        onSelect={onSelect}
      />
    ),
    [onSelect, selectedPath],
  );
  const focusFile = useCallback((path: string) => {
    const buttons = containerRef.current?.querySelectorAll<HTMLButtonElement>(
      "[data-changed-file-path]",
    );
    for (const button of buttons ?? []) {
      if (button.dataset.changedFilePath === path) {
        button.focus();
        break;
      }
    }
  }, []);
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const button = (event.target as HTMLElement).closest<HTMLButtonElement>(
        "[data-changed-file-path]",
      );
      const currentPath = button?.dataset.changedFilePath;
      if (!currentPath) return;
      const currentIndex = paths.indexOf(currentPath);
      const nextIndex = currentIndex + (event.key === "ArrowDown" ? 1 : -1);
      const nextPath = paths[nextIndex];
      if (!nextPath) return;

      event.preventDefault();
      onSelect(nextPath);
      const scroll = listRef.current?.scrollToIndex({
        index: nextIndex,
        animated: false,
        viewPosition: 0.5,
      });
      void Promise.resolve(scroll).then(
        () => focusFile(nextPath),
        () => undefined,
      );
    },
    [focusFile, onSelect, paths],
  );

  return (
    <div
      ref={containerRef}
      aria-label="Changed files"
      className="min-h-0 overflow-hidden border-r border-panel-separator"
      role="listbox"
      onKeyDown={handleKeyDown}
    >
      <LegendList<string>
        ref={listRef}
        className="h-full overflow-x-hidden overscroll-y-contain"
        data={paths}
        estimatedItemSize={CHANGED_FILE_ROW_HEIGHT}
        getFixedItemSize={getChangedFileRowSize}
        keyExtractor={getChangedFileKey}
        renderItem={renderFile}
      />
    </div>
  );
});

export const GitManagerCommitDetail = memo(function GitManagerCommitDetail({
  environmentId,
  cwd,
  commit,
  selectedFilePath,
  onSelectFile,
}: GitManagerCommitDetailProps) {
  const { resolvedTheme } = useTheme();
  const [copiedSha, setCopiedSha] = useState<string | null>(null);
  const [largeDiffOverrideKey, setLargeDiffOverrideKey] = useState<string | null>(null);
  const diffAtom = useMemo(
    () =>
      selectedFilePath === null
        ? null
        : gitManagerEnvironment.getDiff({
            environmentId,
            input: {
              cwd,
              source: { _tag: "commit", sha: commit.sha, path: selectedFilePath },
            },
          }),
    [commit.sha, cwd, environmentId, selectedFilePath],
  );
  const diffQuery = useEnvironmentQuery(diffAtom);
  const diff = diffQuery.data;
  const diffTag = diff?._tag ?? null;
  const diffGeneration = diff?.generation ?? null;
  const diffByteLength = diff?.byteLength ?? 0;
  const diffLongestLineLength = diff?.longestLineLength ?? 0;
  const diffPatch = diff?._tag === "patch" ? diff.patch : null;
  const diffClassification =
    diff === null
      ? null
      : classifyDiffPayload({
          byteLength: diffByteLength,
          longestLineLength: diffLongestLineLength,
          ...(diffTag === "image" ? { kind: "image" as const } : {}),
        });
  const currentDiffKey =
    selectedFilePath === null || diffGeneration === null
      ? null
      : `${commit.sha}\u0000${selectedFilePath}\u0000${diffGeneration}`;
  const largeDiffAllowed = currentDiffKey !== null && largeDiffOverrideKey === currentDiffKey;
  const renderablePatch = useMemo(() => {
    if (
      diffPatch === null ||
      diffClassification === "unrenderable" ||
      (diffClassification === "large-text" && !largeDiffAllowed)
    ) {
      return null;
    }
    return getRenderablePatch(diffPatch, `git-manager-history:${resolvedTheme}`);
  }, [diffClassification, diffPatch, largeDiffAllowed, resolvedTheme]);
  const fileDiffOptions = useMemo(
    () => ({
      collapsed: false,
      diffStyle: "unified" as const,
      theme: resolveDiffThemeName(resolvedTheme),
    }),
    [resolvedTheme],
  );

  const copySha = useCallback(() => {
    const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
    if (!clipboard) return;
    void clipboard.writeText(commit.sha).then(
      () => setCopiedSha(commit.sha),
      () => undefined,
    );
  }, [commit.sha]);
  const showLargeDiff = useCallback(() => {
    if (currentDiffKey !== null) setLargeDiffOverrideKey(currentDiffKey);
  }, [currentDiffKey]);

  return (
    <section
      aria-label={`Commit ${commit.shortSha} details`}
      className="flex min-h-0 flex-1 flex-col"
    >
      <header className="shrink-0 space-y-3 border-b border-border p-4">
        <div className="flex min-w-0 items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="text-pretty break-words text-sm font-semibold text-foreground">
              {commit.subject || "(no subject)"}
            </h2>
            {commit.body.trim().length > 0 ? (
              <p className="mt-1 whitespace-pre-wrap break-words text-xs text-muted-foreground">
                {commit.body}
              </p>
            ) : null}
          </div>
          <button
            type="button"
            aria-label={`Copy commit SHA ${commit.shortSha}`}
            className="inline-flex shrink-0 items-center gap-1 rounded-md border border-border px-2 py-1 font-mono text-xs text-muted-foreground hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
            translate="no"
            onClick={copySha}
          >
            {copiedSha === commit.sha ? (
              <CheckIcon aria-hidden="true" className="size-3" />
            ) : (
              <CopyIcon aria-hidden="true" className="size-3" />
            )}
            {commit.shortSha}
          </button>
        </div>
        {commit.decorations.length > 0 ? (
          <div aria-label="References" className="flex flex-wrap gap-1">
            {commit.decorations.map((decoration) => (
              <span
                key={decoration}
                className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                translate="no"
              >
                {decoration}
              </span>
            ))}
          </div>
        ) : null}
        <div className="grid gap-3 sm:grid-cols-2">
          <IdentityBadge
            label="Author"
            name={commit.authorName}
            email={commit.authorEmail}
            timestampMs={commit.authoredAtMs}
          />
          <IdentityBadge
            label="Committer"
            name={commit.committerName}
            email={commit.committerEmail}
            timestampMs={commit.committedAtMs}
          />
        </div>
      </header>

      <div className="grid min-h-0 flex-1 grid-cols-[minmax(160px,30%)_minmax(0,1fr)]">
        {commit.changedFiles.length > 0 ? (
          <ChangedFileList
            paths={commit.changedFiles}
            selectedPath={selectedFilePath}
            onSelect={onSelectFile}
          />
        ) : (
          <div className="border-r border-panel-separator">
            <p className="p-3 text-xs text-muted-foreground">No changed files.</p>
          </div>
        )}

        <div className="min-h-0 overflow-auto p-2" aria-live="polite">
          {selectedFilePath === null ? (
            <p className="p-3 text-xs text-muted-foreground">
              Select a changed file to view its diff.
            </p>
          ) : diffQuery.isPending && diff === null ? (
            <p role="status" className="p-3 text-xs text-muted-foreground">
              Loading diff…
            </p>
          ) : diffQuery.error !== null && diff === null ? (
            <p className="p-3 text-xs text-destructive">{diffQuery.error}</p>
          ) : diffClassification === "unrenderable" || diffTag === "unrenderable" ? (
            <p className="p-3 text-xs text-muted-foreground">
              This diff is too large to render safely.
            </p>
          ) : diffClassification === "large-text" && diffPatch !== null && !largeDiffAllowed ? (
            <div className="space-y-2 p-3">
              <p className="text-xs text-muted-foreground">
                This is a large text diff and may take time to render.
              </p>
              <button
                type="button"
                className="rounded-md border border-border px-2.5 py-1.5 text-xs font-medium hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
                onClick={showLargeDiff}
              >
                Show diff anyway
              </button>
            </div>
          ) : diffTag === "large-text" ? (
            <p className="p-3 text-xs text-muted-foreground">
              This diff is too large to transfer for rendering.
            </p>
          ) : diffClassification === "image" && diff?._tag === "image" ? (
            <GitManagerStoredImageDiff
              after={buildImageDataUri(diff.after)}
              before={buildImageDataUri(diff.before)}
            />
          ) : renderablePatch?.kind === "files" ? (
            <Suspense fallback={DIFF_PREPARING_FALLBACK}>
              <DiffWorkerPoolProvider>
                <div className="space-y-2">
                  {renderablePatch.files.map((fileDiff) => (
                    <FileDiff
                      key={buildFileDiffRenderKey(fileDiff)}
                      fileDiff={fileDiff}
                      options={fileDiffOptions}
                    />
                  ))}
                </div>
              </DiffWorkerPoolProvider>
            </Suspense>
          ) : renderablePatch?.kind === "raw" ? (
            <div className="space-y-2">
              <p className="text-xs text-muted-foreground">{renderablePatch.reason}</p>
              <pre className="overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/25 p-3 font-mono text-xs">
                {renderablePatch.text}
              </pre>
            </div>
          ) : diff === null ? null : (
            <p className="p-3 text-xs text-muted-foreground">No diff content.</p>
          )}
        </div>
      </div>
    </section>
  );
});
