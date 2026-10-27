import { squashAtomCommandFailure } from "@coddy/client-runtime/state/runtime";
import { projectKey } from "@coddy/client-runtime/state/entities";
import type { EnvironmentId, ScopedProjectRef } from "@coddy/contracts";
import type { SelectedLineRange } from "@pierre/diffs";
import {
  lazy,
  memo,
  Suspense,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { DraftId } from "~/composerDraftStore";
import { useTheme } from "~/hooks/useTheme";
import {
  buildFileDiffRenderKey,
  getRenderablePatch,
  resolveDiffThemeName,
  resolveFileDiffPath,
} from "~/lib/diffRendering";
import { gitManagerEnvironment } from "~/state/gitManager";
import { useEnvironmentQuery } from "~/state/query";
import { useAtomCommand } from "~/state/use-atom-command";

import {
  DEFAULT_GIT_MANAGER_VIEW_STATE,
  type SerializedGitManagerLineSelection,
  useGitManagerStore,
} from "../../../gitManagerStore";
import { AnnotatableCodeView } from "../../diffs/AnnotatableCodeView";
import { GitManagerPartialDiscardDialog } from "../staging/GitManagerPartialDiscardDialog";
import { GitManagerStagingGutter } from "../staging/GitManagerStagingGutter";
import { groupContiguousRuns, resolveSelectedRangeIndices } from "../staging/gitManagerHunkModel";
import {
  createLineSelection,
  type GitManagerLineSelection,
  type GitManagerWireSelection,
  resolveSelectionMutationFailure,
  toWireSelection,
  withLineSelection,
  withRangeSelection,
} from "../staging/gitManagerLineSelection";

const DiffWorkerPoolProvider = lazy(async () => {
  const module = await import("../../DiffWorkerPoolProvider");
  return { default: module.DiffWorkerPoolProvider };
});

const DIFF_PREPARING_FALLBACK = (
  <p className="p-3 text-xs text-muted-foreground" role="status">
    Preparing working-tree diff…
  </p>
);
const renderHeaderPrefix = () => null;

// Sets the busy flag for the length of one mutation. The React Compiler skips the
// body of a `try` without a `catch` when it infers a callback's dependencies, so
// callbacks keep that `try` here.
async function runWhileBusy<T>(
  setBusy: (busy: boolean) => void,
  run: () => Promise<T>,
): Promise<T> {
  setBusy(true);
  try {
    return await run();
  } finally {
    setBusy(false);
  }
}

export type GitManagerPartialArea = "staged" | "unstaged";

export function selectPartialStagingCommand<T>(
  area: GitManagerPartialArea,
  commands: { readonly stagePartial: T; readonly unstagePartial: T },
): T {
  return area === "staged" ? commands.unstagePartial : commands.stagePartial;
}

export function readCurrentWireSelection(
  readSelection: () => GitManagerLineSelection,
  path: string,
  generation: number,
): { readonly selection: GitManagerLineSelection; readonly wire: GitManagerWireSelection } {
  const selection = readSelection();
  return { selection, wire: toWireSelection(selection, path, generation) };
}

function serializeSelection(
  selection: GitManagerLineSelection,
  area: GitManagerPartialArea,
  generation: number,
): SerializedGitManagerLineSelection {
  return {
    type: selection.type,
    basis: selection.basis,
    diverging: [...selection.diverging].toSorted((left, right) => left - right),
    selectable:
      selection.selectable === null
        ? null
        : [...selection.selectable].toSorted((left, right) => left - right),
    area,
    generation,
  };
}

function restoreSelection(
  serialized: SerializedGitManagerLineSelection | undefined,
  area: GitManagerPartialArea,
  generation: number,
  selectable: ReadonlyArray<number>,
): GitManagerLineSelection {
  if (
    serialized === undefined ||
    serialized.area !== area ||
    serialized.generation !== generation
  ) {
    return createLineSelection("none", selectable);
  }
  let selection = createLineSelection(serialized.basis, selectable);
  const defaultSelected = serialized.basis === "all";
  for (const index of serialized.diverging) {
    selection = withLineSelection(selection, index, !defaultSelected);
  }
  return selection;
}

export interface GitManagerDiffPaneProps {
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly projectRef: ScopedProjectRef;
  readonly path: string;
  readonly availableAreas: ReadonlyArray<GitManagerPartialArea>;
  readonly mutationBusy: boolean;
  readonly partialStagingDisabledReason: string | null;
  readonly whitespaceHidden?: boolean;
}

export const GitManagerDiffPane = memo(function GitManagerDiffPane({
  scope,
  projectRef,
  path,
  availableAreas,
  mutationBusy,
  partialStagingDisabledReason,
  whitespaceHidden = false,
}: GitManagerDiffPaneProps) {
  const { environmentId, cwd } = scope;
  const { resolvedTheme } = useTheme();
  const [preferredArea, setPreferredArea] = useState<GitManagerPartialArea>(
    () => availableAreas[0] ?? "unstaged",
  );
  const activeArea = availableAreas.includes(preferredArea)
    ? preferredArea
    : (availableAreas[0] ?? "unstaged");
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [rendererSelection, setRendererSelection] = useState<{
    readonly id: string;
    readonly range: SelectedLineRange;
  } | null>(null);
  const [partialBusy, setPartialBusy] = useState(false);
  const partialBusyRef = useRef(false);
  const [pendingDiscard, setPendingDiscard] = useState<{
    readonly selection: GitManagerLineSelection;
    readonly generation: number;
  } | null>(null);
  const setLineSelection = useGitManagerStore((state) => state.setLineSelection);
  const storeKey = projectKey(projectRef);
  const storedSelection = useGitManagerStore(
    useCallback(
      (state) =>
        (state.byProjectKey[storeKey] ?? DEFAULT_GIT_MANAGER_VIEW_STATE).lineSelectionByPath[path],
      [path, storeKey],
    ),
  );

  const diffAtom = useMemo(
    () =>
      gitManagerEnvironment.getDiff?.({
        environmentId,
        input: {
          cwd,
          source: { _tag: "working-tree", path, staged: activeArea === "staged" },
        },
      }) ?? null,
    [activeArea, cwd, environmentId, path],
  );
  const diffQuery = useEnvironmentQuery(diffAtom);
  // Stale selections and finished partial mutations re-read automatically; no explicit Retry here.
  const revalidateDiff = diffQuery.revalidate;
  const diff = diffQuery.data;
  const patch = diff?._tag === "patch" ? diff.patch : null;
  const renderablePatch = useMemo(
    () =>
      getRenderablePatch(patch ?? undefined, "git-manager-staging", {
        compactPartialHunkOffsets: true,
      }),
    [patch],
  );
  const fileDiff = renderablePatch?.kind === "files" ? (renderablePatch.files[0] ?? null) : null;
  const selectable = useMemo(
    () =>
      fileDiff === null ? [] : groupContiguousRuns(fileDiff).flatMap((run) => [...run.indices]),
    [fileDiff],
  );
  const generation = diff?.generation ?? 0;
  const selection = useMemo(
    () => restoreSelection(storedSelection, activeArea, generation, selectable),
    [activeArea, generation, selectable, storedSelection],
  );
  const selectionRef = useRef(selection);
  useLayoutEffect(() => {
    selectionRef.current = selection;
  }, [selection]);
  const codeViewFiles = useMemo(
    () =>
      fileDiff === null
        ? []
        : [
            {
              fileDiff,
              filePath: resolveFileDiffPath(fileDiff),
              fileKey: buildFileDiffRenderKey(fileDiff),
              collapsed: false,
            },
          ],
    [fileDiff],
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
    () => DraftId.make(`git-manager-staging:${environmentId}:${projectRef.projectId}`),
    [environmentId, projectRef.projectId],
  );
  const stagePartial = useAtomCommand(gitManagerEnvironment.stagePartial, { reportFailure: false });
  const unstagePartial = useAtomCommand(gitManagerEnvironment.unstagePartial, {
    reportFailure: false,
  });
  const handleSelectionChange = useCallback(
    (next: GitManagerLineSelection) => {
      selectionRef.current = next;
      setLineSelection(projectRef, path, serializeSelection(next, activeArea, generation));
    },
    [activeArea, generation, path, projectRef, setLineSelection],
  );
  const handleApplySelection = useCallback(async () => {
    const { selection: snapshot, wire } = readCurrentWireSelection(
      () => {
        const currentState = useGitManagerStore.getState();
        const latestStored = currentState.selectViewState(projectRef).lineSelectionByPath[path];
        return restoreSelection(latestStored, activeArea, generation, selectable);
      },
      path,
      generation,
    );
    const [firstLine, ...remainingLines] = wire.selectedLines;
    if (firstLine === undefined || partialBusyRef.current) return;
    const setBusy = (busy: boolean) => {
      partialBusyRef.current = busy;
      setPartialBusy(busy);
    };
    await runWhileBusy(setBusy, async () => {
      setMutationError(null);
      const command = selectPartialStagingCommand(activeArea, { stagePartial, unstagePartial });
      const result = await command({
        environmentId,
        input: {
          cwd,
          projectId: projectRef.projectId,
          path: wire.path,
          selectedLines: [firstLine, ...remainingLines],
          baseGeneration: wire.baseGeneration,
        },
      });
      if (result._tag === "Failure") {
        const resolution = resolveSelectionMutationFailure(
          snapshot,
          squashAtomCommandFailure(result),
        );
        setMutationError(resolution.message);
        if (resolution.stale) revalidateDiff();
        return;
      }
      setLineSelection(projectRef, path, null);
      revalidateDiff();
    });
  }, [
    activeArea,
    cwd,
    environmentId,
    generation,
    path,
    projectRef,
    revalidateDiff,
    selectable,
    setLineSelection,
    stagePartial,
    unstagePartial,
  ]);
  const handleDiscardOpenChange = useCallback((open: boolean) => {
    if (!open) setPendingDiscard(null);
  }, []);
  const handleDiscardCompleted = useCallback(() => {
    setLineSelection(projectRef, path, null);
    revalidateDiff();
  }, [path, projectRef, revalidateDiff, setLineSelection]);
  const disabledReason = mutationBusy
    ? "A commit is in progress."
    : partialBusy
      ? "Applying the selected lines."
      : whitespaceHidden
        ? "Show whitespace to select individual lines."
        : partialStagingDisabledReason;
  const handleRendererSelectionEnd = useCallback(
    (range: SelectedLineRange | null) => {
      if (range === null || fileDiff === null || disabledReason !== null) return;
      const indices = resolveSelectedRangeIndices(fileDiff, range);
      if (indices === null) return;
      handleSelectionChange(withRangeSelection(selectionRef.current, indices[0], indices[1], true));
      setRendererSelection(null);
    },
    [disabledReason, fileDiff, handleSelectionChange],
  );
  const selectStaged = useCallback(() => {
    setRendererSelection(null);
    setPreferredArea("staged");
  }, []);
  const selectUnstaged = useCallback(() => {
    setRendererSelection(null);
    setPreferredArea("unstaged");
  }, []);
  const openDiscard = useCallback(
    () => setPendingDiscard({ selection: selectionRef.current, generation }),
    [generation],
  );

  return (
    <section
      aria-label={`Diff for ${path}`}
      className="flex min-h-0 flex-1 flex-col border-t border-panel-separator"
    >
      <div className="flex h-8 shrink-0 items-center gap-1 border-b border-border px-2">
        <span className="min-w-0 flex-1 truncate font-mono text-xs">{path}</span>
        {availableAreas.includes("unstaged") ? (
          <button
            aria-pressed={activeArea === "unstaged"}
            className="rounded px-2 py-0.5 text-xs aria-pressed:bg-accent"
            type="button"
            onClick={selectUnstaged}
          >
            Unstaged
          </button>
        ) : null}
        {availableAreas.includes("staged") ? (
          <button
            aria-pressed={activeArea === "staged"}
            className="rounded px-2 py-0.5 text-xs aria-pressed:bg-accent"
            type="button"
            onClick={selectStaged}
          >
            Staged
          </button>
        ) : null}
      </div>
      {mutationError === null ? null : (
        <p aria-live="polite" className="border-b border-border px-2 py-1 text-xs text-destructive">
          {mutationError}
        </p>
      )}
      <div className="flex min-h-0 min-w-0 flex-1">
        {diffQuery.isPending && diff === null ? (
          <p className="p-3 text-xs text-muted-foreground" role="status">
            Loading diff…
          </p>
        ) : diffQuery.error !== null && diff === null ? (
          <p className="p-3 text-xs text-destructive">{diffQuery.error}</p>
        ) : diff?._tag === "large-text" ? (
          <p className="p-3 text-xs text-muted-foreground">
            This diff is too large for partial staging; use whole-file staging.
          </p>
        ) : diff?._tag === "unrenderable" ? (
          <p className="p-3 text-xs text-muted-foreground">This diff cannot be rendered safely.</p>
        ) : diff?._tag === "image" ? (
          <p className="p-3 text-xs text-muted-foreground">
            Binary and image files support whole-file staging only.
          </p>
        ) : fileDiff !== null ? (
          <>
            <GitManagerStagingGutter
              {...(diff?._tag === "patch"
                ? {
                    payload: {
                      byteLength: diff.byteLength,
                      longestLineLength: diff.longestLineLength,
                    },
                  }
                : {})}
              {...(activeArea === "unstaged" ? { onRequestDiscard: openDiscard } : {})}
              area={activeArea}
              disabledReason={disabledReason}
              fileDiff={fileDiff}
              selection={selection}
              onApplySelection={handleApplySelection}
              onSelectionChange={handleSelectionChange}
            />
            <div className="min-h-0 min-w-0 flex-1 overflow-auto p-2">
              <Suspense fallback={DIFF_PREPARING_FALLBACK}>
                <DiffWorkerPoolProvider>
                  <AnnotatableCodeView
                    className="diff-render-surface h-full min-h-0 overflow-auto"
                    composerDraftTarget={composerDraftTarget}
                    enableGutterUtility={false}
                    enableLineSelection={disabledReason === null}
                    files={codeViewFiles}
                    options={codeViewOptions}
                    renderHeaderPrefix={renderHeaderPrefix}
                    sectionId={`git-manager-staging:${activeArea}:${path}`}
                    sectionTitle={`${activeArea === "staged" ? "Staged" : "Unstaged"} ${path}`}
                    selectedLines={disabledReason === null ? rendererSelection : null}
                    onLineSelectionEnd={handleRendererSelectionEnd}
                    onSelectedLinesChange={setRendererSelection}
                  />
                </DiffWorkerPoolProvider>
              </Suspense>
            </div>
          </>
        ) : renderablePatch?.kind === "raw" ? (
          <div className="min-w-0 flex-1 space-y-2 overflow-auto p-2">
            <p className="text-xs text-muted-foreground">{renderablePatch.reason}</p>
            <pre className="whitespace-pre-wrap rounded border border-border bg-muted/25 p-2 font-mono text-xs">
              {renderablePatch.text}
            </pre>
          </div>
        ) : (
          <p className="p-3 text-xs text-muted-foreground">No selectable diff is available.</p>
        )}
      </div>
      {pendingDiscard === null ? null : (
        <GitManagerPartialDiscardDialog
          generation={pendingDiscard.generation}
          open
          path={path}
          projectRef={projectRef}
          scope={scope}
          selection={pendingDiscard.selection}
          onCompleted={handleDiscardCompleted}
          onOpenChange={handleDiscardOpenChange}
          onStale={revalidateDiff}
        />
      )}
    </section>
  );
});
