import type { GitManagerBlockedReason } from "@bibcode/contracts";
import {
  closestCenter,
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { restrictToFirstScrollableAncestor, restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import {
  createElement,
  useCallback,
  useMemo,
  useRef,
  type ComponentProps,
  type ComponentType,
  type ReactNode,
} from "react";

const EMPTY_MULTI_COMMIT_SELECTION: ReadonlyArray<string> = Object.freeze([]);
const COMMIT_DND_MODIFIERS = [restrictToFirstScrollableAncestor, restrictToVerticalAxis];
const COMMIT_KEYBOARD_CODES = Object.freeze({
  start: ["Enter", "Space"],
  cancel: ["Escape"],
  end: ["Enter", "Space"],
});
const commitDndId = (sha: string) => `git-manager-commit:${sha}`;
const insertionDndId = (sha: string) => `git-manager-insertion:${sha}`;
const SortableContextWithoutRequiredChildren = SortableContext as ComponentType<
  Omit<ComponentProps<typeof SortableContext>, "children">
>;

export interface GitManagerCommitDragData {
  readonly type: "commit";
  readonly shas: ReadonlyArray<string>;
}

type BlockableCommitDropTarget = {
  readonly blocked?: GitManagerBlockedReason | null;
};

export type GitManagerCommitDropTarget =
  | ({ readonly type: "branch"; readonly branch: string } & BlockableCommitDropTarget)
  | ({ readonly type: "new-branch" } & BlockableCommitDropTarget)
  | ({ readonly type: "commit"; readonly sha: string } & BlockableCommitDropTarget)
  | ({ readonly type: "insertion"; readonly beforeSha: string | null } & BlockableCommitDropTarget)
  | { readonly type: "other" };

export type GitManagerCommitDropResolution =
  | {
      readonly _tag: "cherry-pick";
      readonly shas: ReadonlyArray<string>;
      readonly branch: string | null;
      readonly createBranch: boolean;
    }
  | {
      readonly _tag: "squash";
      readonly shas: ReadonlyArray<string>;
      readonly targetSha: string;
    }
  | {
      readonly _tag: "reorder";
      readonly shas: ReadonlyArray<string>;
      readonly insertBeforeSha: string | null;
    }
  | { readonly _tag: "blocked"; readonly reason: GitManagerBlockedReason };

export function resolveCommitDropTarget(
  drag: GitManagerCommitDragData,
  over: GitManagerCommitDropTarget,
): GitManagerCommitDropResolution | null {
  if (drag.shas.length === 0 || over.type === "other") return null;
  if (over.blocked) return { _tag: "blocked", reason: over.blocked };
  if (
    (over.type === "commit" && drag.shas.includes(over.sha)) ||
    (over.type === "insertion" && over.beforeSha !== null && drag.shas.includes(over.beforeSha))
  ) {
    return null;
  }

  switch (over.type) {
    case "branch":
      return {
        _tag: "cherry-pick",
        shas: drag.shas,
        branch: over.branch,
        createBranch: false,
      };
    case "new-branch":
      return {
        _tag: "cherry-pick",
        shas: drag.shas,
        branch: null,
        createBranch: true,
      };
    case "commit":
      return { _tag: "squash", shas: drag.shas, targetSha: over.sha };
    case "insertion":
      return { _tag: "reorder", shas: drag.shas, insertBeforeSha: over.beforeSha };
  }
}

export interface GitManagerCommitKeyboardReorderState {
  readonly activeSha: string;
  readonly overSha: string;
  readonly dropped: boolean;
}

export function advanceCommitKeyboardReorder(
  state: GitManagerCommitKeyboardReorderState,
  key: "ArrowUp" | "ArrowDown" | "Enter",
  loadedOrder: ReadonlyArray<string>,
): GitManagerCommitKeyboardReorderState {
  if (key === "Enter") return state.dropped ? state : { ...state, dropped: true };
  const currentIndex = loadedOrder.indexOf(state.overSha);
  if (currentIndex < 0) return state;
  const nextIndex = currentIndex + (key === "ArrowDown" ? 1 : -1);
  const nextSha = loadedOrder[nextIndex];
  return nextSha === undefined ? state : { ...state, overSha: nextSha };
}

export function useGitManagerCommitDragSource(sha: string, disabled = false) {
  const sortable = useSortable({
    id: commitDndId(sha),
    data: { type: "commit", sha },
    disabled,
  });
  return {
    attributes: sortable.attributes,
    isDragging: sortable.isDragging,
    listeners: sortable.listeners,
    setNodeRef: sortable.setNodeRef,
    transform: CSS.Transform.toString(sortable.transform),
  };
}

export function GitManagerCommitInsertionTarget({ beforeSha }: { readonly beforeSha: string }) {
  const droppable = useDroppable({
    id: insertionDndId(beforeSha),
    data: { type: "insertion", beforeSha },
  });
  return createElement("span", {
    ref: droppable.setNodeRef,
    "aria-hidden": true,
    className: `pointer-events-auto absolute inset-x-0 -top-1 z-10 h-2${droppable.isOver ? " bg-primary/35" : ""}`,
    "data-commit-insertion-before": beforeSha,
  });
}

export interface GitManagerCommitDndContextProps {
  readonly children: ReactNode;
  readonly commitShas: ReadonlyArray<string>;
  readonly multiCommitSelection?: ReadonlyArray<string>;
  readonly onCommitDrop?: (resolution: GitManagerCommitDropResolution) => void;
}

// One drag gesture: the commits picked up at drag start and their drop.
function useCommitDragSession(
  multiCommitSelection: ReadonlyArray<string>,
  onCommitDrop: ((resolution: GitManagerCommitDropResolution) => void) | undefined,
) {
  const activeDragShasRef = useRef<ReadonlyArray<string>>(EMPTY_MULTI_COMMIT_SELECTION);
  const clearDrag = useCallback(() => {
    activeDragShasRef.current = EMPTY_MULTI_COMMIT_SELECTION;
  }, []);
  const handleDragStart = useCallback(
    (event: DragStartEvent) => {
      const sha = event.active.data.current?.["sha"];
      if (typeof sha !== "string") return;
      activeDragShasRef.current = multiCommitSelection.includes(sha) ? multiCommitSelection : [sha];
    },
    [multiCommitSelection],
  );
  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const overData = event.over?.data.current;
      const shas = activeDragShasRef.current;
      clearDrag();
      if (onCommitDrop === undefined || overData === undefined || shas.length === 0) return;

      let target: GitManagerCommitDropTarget;
      if (overData["type"] === "commit" && typeof overData["sha"] === "string") {
        // The activator is the event that started this drag.
        target = event.activatorEvent.type.startsWith("key")
          ? { type: "insertion", beforeSha: overData["sha"] }
          : { type: "commit", sha: overData["sha"] };
      } else if (overData["type"] === "insertion") {
        const beforeSha = overData["beforeSha"];
        target = { type: "insertion", beforeSha: typeof beforeSha === "string" ? beforeSha : null };
      } else {
        target = { type: "other" };
      }
      const resolution = resolveCommitDropTarget({ type: "commit", shas }, target);
      if (resolution !== null) onCommitDrop(resolution);
    },
    [clearDrag, onCommitDrop],
  );
  return { clearDrag, handleDragEnd, handleDragStart };
}

export function GitManagerCommitDndContext({
  children,
  commitShas,
  multiCommitSelection = EMPTY_MULTI_COMMIT_SELECTION,
  onCommitDrop,
}: GitManagerCommitDndContextProps) {
  const { clearDrag, handleDragEnd, handleDragStart } = useCommitDragSession(
    multiCommitSelection,
    onCommitDrop,
  );
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, { keyboardCodes: COMMIT_KEYBOARD_CODES }),
  );
  const dndIds = useMemo(() => commitShas.map(commitDndId), [commitShas]);

  return createElement(
    DndContext,
    {
      collisionDetection: closestCenter,
      modifiers: COMMIT_DND_MODIFIERS,
      sensors,
      onDragCancel: clearDrag,
      onDragEnd: handleDragEnd,
      onDragStart: handleDragStart,
    },
    createElement(
      SortableContextWithoutRequiredChildren,
      { items: dndIds, strategy: verticalListSortingStrategy },
      children,
    ),
  );
}
