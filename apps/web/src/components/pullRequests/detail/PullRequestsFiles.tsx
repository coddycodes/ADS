import { useShallow } from "zustand/react/shallow";
import {
  PullRequestsInlineComposer,
  PullRequestsPendingComment,
} from "../review/PullRequestsInlineComposer";
import {
  PullRequestsApplySuggestionsButton,
  PullRequestsSuggestionSelectionContext,
} from "../shared/PullRequestsSuggestionBlock";
import { PullRequestsPendingReviewBar } from "../review/PullRequestsPendingReviewBar";
import type {
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsFiles as Files,
  ScopedProjectRef,
  PullRequestsTimeline,
} from "@coddy/contracts";
import { projectKey } from "@coddy/client-runtime/state/entities";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import { FileDiffIcon, FileMinusIcon, FilePlusIcon, FolderIcon, MoveRightIcon } from "lucide-react";
import { memo, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useClientSettings } from "../../../hooks/useSettings";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { DiffWorkerPoolProvider } from "../../DiffWorkerPoolProvider";
import { PullRequestsExternalLink } from "../shared/PullRequestsMarkdown";
import { fileTree, type FileTreeRow } from "./pullRequestsDetail.logic";
import { PullRequestsReviewFile } from "../review/PullRequestsReviewFile";
import { usePullRequestsFileAnchor, usePullRequestsNavigation } from "./pullRequestsNavigation";
export interface PullRequestsFilesProps {
  files: Files;
  timeline?: PullRequestsTimeline;
  detail: PullRequestsDetail;
  projectRef: ScopedProjectRef;
  context: Extract<PullRequestsContext, { status: "available" }>;
  /**
   * The bulk apply-suggestions control, the pending review bar, and every
   * per-file review action (threads, pending comments, the inline composer)
   * only ever act on a live detail, never a snapshot. Omitting this falls
   * back to `detail`, so callers that have not adopted the split keep
   * acting as before.
   */
  live?: PullRequestsDetail | null;
}
const EMPTY_TIMELINE: PullRequestsTimeline = { items: [], truncated: false };
const EMPTY_VIEWED: readonly string[] = [];
const rowKey = (row: FileTreeRow) => `${row.kind}:${row.path}`;
const fileKey = (file: Files["files"][number]) => file.path;
const treeRowSize = () => 56;
const FileTreeEntry = memo(function FileTreeEntry({
  row,
  selected,
  viewed,
  onSelect,
  onToggleViewed,
}: {
  row: FileTreeRow;
  selected: boolean;
  viewed: boolean;
  onSelect: (path: string) => void;
  onToggleViewed: (path: string) => void;
}) {
  if (row.kind === "folder")
    return (
      <div
        className="flex h-14 items-center gap-2 border-b border-border px-3 text-xs font-medium"
        style={{ paddingInlineStart: 12 + row.depth * 12 }}
      >
        <FolderIcon aria-hidden="true" className="size-4 shrink-0" />
        {row.path.split("/").at(-1)}
      </div>
    );
  const Icon =
    row.file.changeType === "added"
      ? FilePlusIcon
      : row.file.changeType === "removed"
        ? FileMinusIcon
        : row.file.changeType === "renamed" || row.file.changeType === "copied"
          ? MoveRightIcon
          : FileDiffIcon;
  return (
    <div
      className={`flex h-14 items-center gap-2 border-b border-border px-3 ${selected ? "bg-accent" : ""}`}
      style={{ paddingInlineStart: 12 + row.depth * 12 }}
    >
      <button
        type="button"
        data-file-path={row.path}
        aria-current={selected ? "true" : undefined}
        className="min-w-0 flex-1 cursor-pointer text-left text-xs hover:underline"
        onClick={() => onSelect(row.path)}
      >
        <span className="flex items-center gap-1.5">
          <span role="img" aria-label={row.file.changeType}>
            <Icon aria-hidden="true" className="size-4" />
          </span>
          <span className="truncate font-mono" title={row.path}>
            {row.path}
          </span>
        </span>
        <span className="text-green-700 dark:text-green-400">+{row.file.additions}</span>{" "}
        <span className="text-red-700 dark:text-red-400">−{row.file.deletions}</span>
      </button>
      <label className="flex cursor-pointer items-center gap-1 text-xs">
        <input
          type="checkbox"
          aria-label={`Viewed ${row.path}`}
          checked={viewed}
          onChange={() => onToggleViewed(row.path)}
        />
        <span className="sr-only">Viewed</span>
      </label>
    </div>
  );
});
function UnanchoredReviewDrafts({
  paths,
  detail,
  projectRef,
  live = true,
}: {
  paths: ReadonlyMap<string, number>;
  detail: PullRequestsDetail;
  projectRef: ScopedProjectRef;
  /**
   * Editing or submitting these drafts only ever acts on a live detail,
   * never a snapshot. Defaults to `true` so callers that have not adopted
   * the split keep acting as before.
   */
  live?: boolean;
}) {
  const pending = usePullRequestsStore(
    useShallow((s) =>
      s
        .selectDraft(projectRef, detail.number)
        .pendingReview.filter((comment) => !paths.has(comment.path)),
    ),
  );
  const drafts = usePullRequestsStore(
    useShallow((s) =>
      s
        .selectDraft(projectRef, detail.number)
        .inlineDrafts.filter((comment) => !paths.has(comment.path)),
    ),
  );
  if (pending.length === 0 && drafts.length === 0) return null;
  return (
    <section
      aria-label="Drafts outside the current diff"
      className="max-h-80 shrink-0 space-y-3 overflow-auto border-b border-border p-3"
      data-text-surface="background"
    >
      <p className="text-sm">
        These files are no longer in this diff. Review or remove their saved comments before
        submitting.
      </p>
      {pending.map((comment) => (
        <div key={comment.id}>
          <p className="break-all font-mono text-xs">
            {comment.path}:{comment.line}
          </p>
          <PullRequestsPendingComment
            comment={comment}
            projectRef={projectRef}
            number={detail.number}
            permission={detail.permissions.review}
            live={live}
          />
        </div>
      ))}
      {drafts.map((draft) => (
        <PullRequestsInlineComposer
          key={draft.id}
          draftId={draft.id}
          projectRef={projectRef}
          number={detail.number}
          permission={detail.permissions.review}
          headSha={detail.headSha}
          patch=""
          live={live}
        />
      ))}
    </section>
  );
}
export const PullRequestsFiles = memo(function PullRequestsFiles({
  files,
  timeline = EMPTY_TIMELINE,
  detail,
  live = detail,
  projectRef,
  context,
}: PullRequestsFilesProps) {
  const isLive = live !== null;
  const navigation = usePullRequestsNavigation();
  const anchorPath = usePullRequestsFileAnchor();
  const initialIgnoreWhitespace = useClientSettings((settings) => settings.diffIgnoreWhitespace);
  const [ignoreWhitespace, setIgnoreWhitespace] = useState(initialIgnoreWhitespace);
  const [scrollFailed, setScrollFailed] = useState(false);
  const list = useRef<LegendListRef | null>(null);
  const key = projectKey(projectRef);
  const viewed = usePullRequestsStore(
    (state) => state.byProjectKey[key]?.viewedFiles[detail.number] ?? EMPTY_VIEWED,
  );
  const viewedSet = useMemo(() => new Set(viewed), [viewed]);
  const rows = useMemo(() => fileTree(files.files), [files.files]);
  const indices = useMemo(
    () => new Map(files.files.map((file, index) => [file.path, index])),
    [files.files],
  );
  const scrollToFile = useCallback(
    (path: string) => {
      const index = indices.get(path);
      if (index !== undefined)
        void list.current?.scrollToIndex({ index, animated: false, viewPosition: 0 }).then(
          () => setScrollFailed(false),
          () => setScrollFailed(true),
        );
    },
    [indices],
  );
  useEffect(() => {
    if (anchorPath !== null) scrollToFile(anchorPath);
  }, [anchorPath, scrollToFile]);
  const selectFile = useCallback(
    (path: string) => {
      scrollToFile(path);
      navigation.setFileAnchor(path);
    },
    [navigation, scrollToFile],
  );
  const toggleViewed = useCallback(
    (path: string) =>
      usePullRequestsStore.getState().toggleViewedFile(projectRef, detail.number, path),
    [detail.number, projectRef],
  );
  const renderTreeRow = useCallback(
    ({ item }: { item: FileTreeRow }) => (
      <FileTreeEntry
        row={item}
        selected={item.path === anchorPath}
        viewed={viewedSet.has(item.path)}
        onSelect={selectFile}
        onToggleViewed={toggleViewed}
      />
    ),
    [anchorPath, selectFile, toggleViewed, viewedSet],
  );
  const threadsByPath = useMemo(() => {
    const map = new Map<
      string,
      Extract<PullRequestsTimeline["items"][number], { kind: "thread" }>[]
    >();
    for (const item of timeline.items)
      if (item.kind === "thread") {
        const threads = map.get(item.path);
        if (threads) threads.push(item);
        else map.set(item.path, [item]);
      }
    return map;
  }, [timeline.items]);
  const [selectedSuggestions, setSelectedSuggestions] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const applicableSuggestions = useMemo(() => {
    const ids = new Set<string>();
    for (const threads of threadsByPath.values())
      for (const thread of threads)
        for (const comment of thread.comments) {
          if (comment.suggestion?.applicable && comment.suggestion.id !== null)
            ids.add(comment.suggestion.id);
        }
    return ids;
  }, [threadsByPath]);
  const selected = useMemo(
    () => new Set([...selectedSuggestions].filter((id) => applicableSuggestions.has(id))),
    [selectedSuggestions, applicableSuggestions],
  );
  const toggleSuggestion = useCallback(
    (id: string, checked: boolean) =>
      setSelectedSuggestions((current) => {
        const next = new Set(current);
        if (checked) next.add(id);
        else next.delete(id);
        return next;
      }),
    [],
  );
  const selection = useMemo(
    () => ({ selected, toggle: toggleSuggestion }),
    [selected, toggleSuggestion],
  );
  const appliedSuggestions = useCallback(
    (ids: readonly string[]) =>
      setSelectedSuggestions((current) => {
        const next = new Set(current);
        for (const id of ids) next.delete(id);
        return next;
      }),
    [],
  );
  const renderFile = useCallback(
    ({ item }: { item: Files["files"][number] }) => (
      <PullRequestsReviewFile
        detail={detail}
        context={context}
        projectRef={projectRef}
        threads={threadsByPath.get(item.path)}
        file={item}
        hostUrl={detail.url}
        viewed={viewedSet.has(item.path)}
        onToggleViewed={toggleViewed}
        ignoreWhitespace={ignoreWhitespace}
        live={isLive}
      />
    ),
    [detail, context, projectRef, threadsByPath, ignoreWhitespace, toggleViewed, viewedSet, isLive],
  );
  return (
    <PullRequestsSuggestionSelectionContext value={selection}>
      <section
        aria-label={context.capabilities.vocabulary.filesChanged}
        className="flex min-h-0 flex-1 flex-col"
        data-text-surface="background"
      >
        <PullRequestsPendingReviewBar
          detail={detail}
          live={live}
          context={context}
          projectRef={projectRef}
        />
        <header className="flex flex-wrap items-center justify-between gap-3 border-b border-panel-separator px-4 py-3 text-xs">
          <span>
            {files.files.length} files{" "}
            <span className="text-green-700 dark:text-green-400">+{detail.additions}</span>{" "}
            <span className="text-red-700 dark:text-red-400">−{detail.deletions}</span>
          </span>
          {selected.size > 1 ? (
            <PullRequestsApplySuggestionsButton
              suggestionIds={[...selected]}
              permission={detail.permissions.applySuggestion}
              label={`Apply ${selected.size} selected`}
              onApplied={appliedSuggestions}
              live={isLive}
            />
          ) : null}
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              aria-label="Ignore whitespace"
              checked={ignoreWhitespace}
              onChange={(event) => setIgnoreWhitespace(event.currentTarget.checked)}
            />
            Ignore whitespace
          </label>
        </header>
        <UnanchoredReviewDrafts
          paths={indices}
          detail={detail}
          projectRef={projectRef}
          live={isLive}
        />
        {files.truncated ? (
          <p className="p-3 text-sm">
            <PullRequestsExternalLink href={detail.url}>
              More changes are on the host page
            </PullRequestsExternalLink>
          </p>
        ) : null}
        {scrollFailed ? (
          <p role="alert" className="p-3 text-sm">
            Could not scroll to the file. Select it again.
          </p>
        ) : null}
        {files.files.length === 0 ? (
          <p className="p-4 text-sm text-muted-foreground">No files changed</p>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col @3xl/pull-request:flex-row">
            <nav
              aria-label="Changed files"
              className="flex h-48 shrink-0 flex-col border-b border-border @3xl/pull-request:h-auto @3xl/pull-request:w-64 @3xl/pull-request:border-r @3xl/pull-request:border-b-0"
            >
              <LegendList
                data={rows}
                keyExtractor={rowKey}
                renderItem={renderTreeRow}
                extraData={renderTreeRow}
                recycleItems={false}
                estimatedItemSize={56}
                getFixedItemSize={treeRowSize}
                className="min-h-0 flex-1"
              />
            </nav>
            <Suspense
              fallback={
                <p role="status" className="p-4 text-sm">
                  Preparing diffs…
                </p>
              }
            >
              <DiffWorkerPoolProvider>
                <LegendList
                  ref={list}
                  data={files.files}
                  keyExtractor={fileKey}
                  renderItem={renderFile}
                  extraData={renderFile}
                  recycleItems={false}
                  estimatedItemSize={300}
                  initialScrollIndex={anchorPath === null ? 0 : (indices.get(anchorPath) ?? 0)}
                  className="min-h-0 min-w-0 flex-1"
                />
              </DiffWorkerPoolProvider>
            </Suspense>
          </div>
        )}
      </section>
    </PullRequestsSuggestionSelectionContext>
  );
});
