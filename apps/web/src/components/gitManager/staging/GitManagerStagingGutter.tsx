import type { GitManagerImageDiffSide } from "@bibcode/contracts";
import type { FileDiffMetadata } from "@pierre/diffs";
import { memo, useCallback, useMemo, useRef, type KeyboardEvent, type PointerEvent } from "react";

import { classifyDiffPayload, type DiffPayloadMeasurements } from "../history/diffLadder";
import { GitManagerStoredImageDiff } from "../diff/GitManagerImageDiffModeContext";
import { buildImageDataUri } from "../diff/gitManagerImageDiff.logic";
import {
  type GitManagerContiguousRun,
  type GitManagerSelectableDiffLine,
  groupContiguousRuns,
  resolveHunkHandleState,
  withToggleHunkSelection,
} from "./gitManagerHunkModel";
import {
  type GitManagerLineSelection,
  isLineSelected,
  withRangeSelection,
  withToggleLine,
} from "./gitManagerLineSelection";

export type GitManagerStagingDiffKind = "text" | "binary" | "submodule" | "unrenderable";
const DISABLED_REASON_ID = "git-manager-staging-gutter-disabled-reason";

export interface GitManagerStagingImagePayload extends DiffPayloadMeasurements {
  readonly kind: "image";
  readonly before: GitManagerImageDiffSide;
  readonly after: GitManagerImageDiffSide;
}

export interface GitManagerStagingGutterProps {
  readonly fileDiff: FileDiffMetadata;
  readonly selection: GitManagerLineSelection;
  readonly onSelectionChange: (next: GitManagerLineSelection) => void;
  readonly disabledReason: string | null;
  readonly area?: "staged" | "unstaged";
  readonly diffKind?: GitManagerStagingDiffKind;
  readonly payload?: DiffPayloadMeasurements | GitManagerStagingImagePayload;
  readonly onApplySelection?: () => void;
  readonly onRequestDiscard?: () => void;
}

interface DragSelection {
  readonly anchor: number;
  readonly selected: boolean;
  readonly origin: GitManagerLineSelection;
}

export function resolveStagingGutterUnavailableReason({
  fileDiff,
  diffKind = "text",
  payload,
}: Pick<GitManagerStagingGutterProps, "fileDiff" | "diffKind" | "payload">): string | null {
  if (fileDiff.type === "rename-pure" || fileDiff.type === "rename-changed") {
    return "Renamed files support whole-file staging only.";
  }
  if (diffKind === "binary") return "Binary files support whole-file staging only.";
  if (diffKind === "submodule") return "Submodules support whole-file staging only.";
  if (diffKind === "unrenderable") return "This diff cannot be rendered safely.";
  if (payload !== undefined) {
    const classification = classifyDiffPayload(payload);
    if (classification === "unrenderable") return "This diff cannot be rendered safely.";
    if (classification === "large-text") {
      return "This diff is too large for partial staging; use whole-file staging.";
    }
  }
  return null;
}

interface GutterLineControlProps {
  readonly line: GitManagerSelectableDiffLine;
  readonly selected: boolean;
  readonly disabledReason: string | null;
  readonly onPointerDownLine: (index: number, event: PointerEvent<HTMLButtonElement>) => void;
  readonly onPointerEnterLine: (index: number) => void;
  readonly onKeyDownLine: (index: number, event: KeyboardEvent<HTMLButtonElement>) => void;
}

const GutterLineControl = memo(function GutterLineControl({
  line,
  selected,
  disabledReason,
  onPointerDownLine,
  onPointerEnterLine,
  onKeyDownLine,
}: GutterLineControlProps) {
  const handlePointerDown = useCallback(
    (event: PointerEvent<HTMLButtonElement>) => onPointerDownLine(line.index, event),
    [line.index, onPointerDownLine],
  );
  const handlePointerEnter = useCallback(
    () => onPointerEnterLine(line.index),
    [line.index, onPointerEnterLine],
  );
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>) => onKeyDownLine(line.index, event),
    [line.index, onKeyDownLine],
  );
  return (
    <button
      aria-checked={selected}
      aria-describedby={disabledReason === null ? undefined : DISABLED_REASON_ID}
      aria-label={`Toggle line ${line.lineNumber}, ${line.side}`}
      className="flex h-5 min-w-0 items-center gap-1 rounded-sm px-1 font-mono text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
      data-line-index={line.index}
      disabled={disabledReason !== null}
      role="checkbox"
      title={disabledReason ?? undefined}
      type="button"
      onKeyDown={handleKeyDown}
      onPointerDown={handlePointerDown}
      onPointerEnter={handlePointerEnter}
      onPointerMove={handlePointerEnter}
    >
      <span aria-hidden="true" className="inline-flex size-3 items-center justify-center border">
        {selected ? "✓" : ""}
      </span>
      <span>{line.side === "additions" ? "+" : "−"}</span>
      <span>{line.lineNumber}</span>
    </button>
  );
});

interface GutterRunControlProps {
  readonly run: GitManagerContiguousRun;
  readonly selection: GitManagerLineSelection;
  readonly disabledReason: string | null;
  readonly onToggleRun: (run: GitManagerContiguousRun) => void;
  readonly onPointerDownLine: GutterLineControlProps["onPointerDownLine"];
  readonly onPointerEnterLine: GutterLineControlProps["onPointerEnterLine"];
  readonly onKeyDownLine: GutterLineControlProps["onKeyDownLine"];
}

function runSelectionEqual(
  previous: Readonly<GutterRunControlProps>,
  next: Readonly<GutterRunControlProps>,
): boolean {
  if (
    previous.run !== next.run ||
    previous.disabledReason !== next.disabledReason ||
    previous.onToggleRun !== next.onToggleRun ||
    previous.onPointerDownLine !== next.onPointerDownLine ||
    previous.onPointerEnterLine !== next.onPointerEnterLine ||
    previous.onKeyDownLine !== next.onKeyDownLine
  ) {
    return false;
  }
  return previous.run.indices.every(
    (index) => isLineSelected(previous.selection, index) === isLineSelected(next.selection, index),
  );
}

const GutterRunControl = memo(function GutterRunControl({
  run,
  selection,
  disabledReason,
  onToggleRun,
  onPointerDownLine,
  onPointerEnterLine,
  onKeyDownLine,
}: GutterRunControlProps) {
  const state = resolveHunkHandleState(selection, run);
  const firstLine = run.lines[0];
  const handleToggleRun = useCallback(() => onToggleRun(run), [onToggleRun, run]);
  return (
    <div className="border-b border-border/50 py-0.5" data-hunk-start={run.start}>
      <button
        aria-checked={state === "partial" ? "mixed" : state === "all"}
        aria-describedby={disabledReason === null ? undefined : DISABLED_REASON_ID}
        aria-label={`Toggle changed-line run starting at line ${firstLine?.lineNumber ?? 0}`}
        className="flex h-5 w-full items-center gap-1 rounded-sm px-1 text-xs text-muted-foreground outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
        disabled={disabledReason !== null}
        role="checkbox"
        title={disabledReason ?? undefined}
        type="button"
        onClick={handleToggleRun}
      >
        <span aria-hidden="true" className="inline-flex size-3 items-center justify-center border">
          {state === "all" ? "✓" : state === "partial" ? "−" : ""}
        </span>
        <span>Changed lines</span>
      </button>
      {run.lines.map((line) => (
        <GutterLineControl
          disabledReason={disabledReason}
          key={`${line.side}:${line.index}`}
          line={line}
          selected={isLineSelected(selection, line.index)}
          onKeyDownLine={onKeyDownLine}
          onPointerDownLine={onPointerDownLine}
          onPointerEnterLine={onPointerEnterLine}
        />
      ))}
    </div>
  );
}, runSelectionEqual);

const GitManagerTextStagingGutter = memo(function GitManagerTextStagingGutter({
  fileDiff,
  selection,
  onSelectionChange,
  disabledReason,
  area = "unstaged",
  diffKind = "text",
  payload,
  onApplySelection,
  onRequestDiscard,
}: GitManagerStagingGutterProps) {
  const unavailableReason = resolveStagingGutterUnavailableReason({
    fileDiff,
    diffKind,
    ...(payload === undefined ? {} : { payload }),
  });
  const runs = useMemo(() => groupContiguousRuns(fileDiff), [fileDiff]);
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const dragRef = useRef<DragSelection | null>(null);
  const anchorRef = useRef<number | null>(null);

  const handlePointerDownLine = useCallback(
    (index: number, event: PointerEvent<HTMLButtonElement>) => {
      if (disabledReason !== null) return;
      event.preventDefault();
      const current = selectionRef.current;
      const selected = !isLineSelected(current, index);
      dragRef.current = { anchor: index, selected, origin: current };
      anchorRef.current = index;
      onSelectionChange(withRangeSelection(current, index, index, selected));
    },
    [disabledReason, onSelectionChange],
  );
  const handlePointerEnterLine = useCallback(
    (index: number) => {
      const drag = dragRef.current;
      if (drag === null || disabledReason !== null) return;
      onSelectionChange(withRangeSelection(drag.origin, drag.anchor, index, drag.selected));
    },
    [disabledReason, onSelectionChange],
  );
  const endDrag = useCallback(() => {
    dragRef.current = null;
  }, []);
  const handleKeyDownLine = useCallback(
    (index: number, event: KeyboardEvent<HTMLButtonElement>) => {
      if (event.key !== " " || disabledReason !== null) return;
      event.preventDefault();
      const current = selectionRef.current;
      if (event.shiftKey && anchorRef.current !== null) {
        onSelectionChange(withRangeSelection(current, anchorRef.current, index, true));
      } else {
        anchorRef.current = index;
        onSelectionChange(withToggleLine(current, index));
      }
    },
    [disabledReason, onSelectionChange],
  );
  const handleToggleRun = useCallback(
    (run: GitManagerContiguousRun) => {
      if (disabledReason === null) {
        onSelectionChange(withToggleHunkSelection(selectionRef.current, run));
      }
    },
    [disabledReason, onSelectionChange],
  );

  if (unavailableReason !== null) {
    return (
      <p className="border border-border bg-muted/30 p-2 text-xs text-muted-foreground" role="note">
        {unavailableReason}
      </p>
    );
  }

  const actionLabel = area === "staged" ? "Unstage selected lines" : "Stage selected lines";
  const selectionDisabled = disabledReason !== null || selection.type === "none";
  return (
    <aside
      aria-label="Partial staging selection gutter"
      className="w-32 shrink-0 overflow-y-auto border-r border-panel-separator bg-background"
      data-staging-gutter="true"
      onPointerLeave={endDrag}
      onPointerUp={endDrag}
    >
      <div className="sticky top-0 z-10 space-y-1 border-b border-border bg-background p-1">
        {onApplySelection === undefined ? null : (
          <button
            aria-describedby={disabledReason === null ? undefined : DISABLED_REASON_ID}
            className="min-h-6 w-full rounded border border-input px-1 py-0.5 text-xs font-medium outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            disabled={selectionDisabled}
            title={disabledReason ?? undefined}
            type="button"
            onClick={onApplySelection}
          >
            {actionLabel}
          </button>
        )}
        {area === "staged" || onRequestDiscard === undefined ? null : (
          <button
            aria-describedby={disabledReason === null ? undefined : DISABLED_REASON_ID}
            className="min-h-6 w-full rounded border border-destructive/50 px-1 py-0.5 text-xs font-medium text-destructive outline-none hover:bg-destructive/10 focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            disabled={selectionDisabled}
            type="button"
            onClick={onRequestDiscard}
          >
            Discard selected lines…
          </button>
        )}
        {disabledReason === null ? null : (
          <p className="text-xs text-muted-foreground" id={DISABLED_REASON_ID} role="status">
            {disabledReason}
          </p>
        )}
      </div>
      {runs.map((run) => (
        <GutterRunControl
          disabledReason={disabledReason}
          key={`${run.start}:${run.end}`}
          run={run}
          selection={selection}
          onKeyDownLine={handleKeyDownLine}
          onPointerDownLine={handlePointerDownLine}
          onPointerEnterLine={handlePointerEnterLine}
          onToggleRun={handleToggleRun}
        />
      ))}
    </aside>
  );
});

function isImagePayload(
  payload: DiffPayloadMeasurements | GitManagerStagingImagePayload | undefined,
): payload is GitManagerStagingImagePayload {
  return (
    payload !== undefined &&
    classifyDiffPayload(payload) === "image" &&
    "before" in payload &&
    "after" in payload
  );
}

export const GitManagerStagingGutter = memo(function GitManagerStagingGutter(
  props: GitManagerStagingGutterProps,
) {
  if (isImagePayload(props.payload)) {
    return (
      <GitManagerStoredImageDiff
        after={buildImageDataUri(props.payload.after)}
        before={buildImageDataUri(props.payload.before)}
      />
    );
  }
  return <GitManagerTextStagingGutter {...props} />;
});
