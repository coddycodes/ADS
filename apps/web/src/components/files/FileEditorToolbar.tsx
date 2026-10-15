import { Code2, Eye, LoaderCircle, Redo2, Save, Undo2 } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";

import { Button } from "../ui/button";
import { Toggle } from "../ui/toggle";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

import type { FileSavePhase } from "./fileSaveCoordinator";

const SAVED_INDICATOR_DURATION_MS = 1_500;

export interface FileEditorMarkdownView {
  readonly rendered: boolean;
  readonly onRenderedChange: (rendered: boolean) => void;
}

export interface FileEditorToolbarProps {
  readonly savePhase: FileSavePhase;
  readonly confirmedRevision: number;
  readonly canSave: boolean;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly cleanStatus: string | null;
  readonly markdownView?: FileEditorMarkdownView | undefined;
  readonly onSave: () => void;
  readonly onUndo: () => void;
  readonly onRedo: () => void;
}

function ToolbarAction({
  label,
  tooltip,
  disabled,
  className,
  onClick,
  children,
}: {
  readonly label: string;
  readonly tooltip: string;
  readonly disabled: boolean;
  readonly className?: string;
  readonly onClick: () => void;
  readonly children: ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            aria-label={disabled ? label : undefined}
            className="inline-flex"
            tabIndex={disabled ? 0 : undefined}
          >
            <Button
              aria-label={label}
              className={className}
              disabled={disabled}
              size="icon-xs"
              variant="ghost"
              onClick={onClick}
            >
              {children}
            </Button>
          </span>
        }
      />
      <TooltipPopup>{tooltip}</TooltipPopup>
    </Tooltip>
  );
}

export function FileEditorToolbar({
  savePhase,
  confirmedRevision,
  canSave,
  canUndo,
  canRedo,
  cleanStatus,
  markdownView,
  onSave,
  onUndo,
  onRedo,
}: FileEditorToolbarProps) {
  const [observedSave, setObservedSave] = useState({ confirmedRevision, savePhase });
  // The revision whose save the notice shows; each newer save restarts its countdown.
  const [savedRevision, setSavedRevision] = useState<number | null>(null);
  if (
    observedSave.confirmedRevision !== confirmedRevision ||
    observedSave.savePhase !== savePhase
  ) {
    setObservedSave({ confirmedRevision, savePhase });
    if (savePhase !== "clean") {
      setSavedRevision(null);
    } else if (confirmedRevision > observedSave.confirmedRevision) {
      setSavedRevision(confirmedRevision);
    }
  }

  useEffect(() => {
    if (savedRevision === null) return;
    const timeout = setTimeout(() => setSavedRevision(null), SAVED_INDICATOR_DURATION_MS);
    return () => clearTimeout(timeout);
  }, [savedRevision]);
  const showSaved = savedRevision !== null;

  const status =
    savePhase === "pending"
      ? "Unsaved changes"
      : savePhase === "saving"
        ? "Saving…"
        : savePhase === "failed"
          ? "Save failed — retry"
          : showSaved
            ? "Saved"
            : cleanStatus;
  const statusClassName =
    savePhase === "failed" ? "ml-2 text-xs text-destructive" : "ml-2 text-xs text-muted-foreground";
  const saveActionClassName =
    savePhase === "failed"
      ? "text-destructive hover:text-destructive"
      : canSave
        ? "text-foreground hover:text-foreground"
        : "text-muted-foreground hover:text-muted-foreground";

  return (
    <div
      className="flex h-9 min-h-9 shrink-0 items-center gap-1 border-b border-panel-separator bg-background px-3"
      data-file-editor-toolbar
    >
      <ToolbarAction
        className={saveActionClassName}
        disabled={!canSave}
        label="Save file"
        tooltip="Save file (Ctrl/Cmd+S)"
        onClick={onSave}
      >
        {savePhase === "saving" ? (
          <LoaderCircle className="animate-spin text-current" />
        ) : (
          <Save className="text-current" />
        )}
      </ToolbarAction>
      <ToolbarAction
        className={
          canUndo
            ? "text-foreground hover:text-foreground"
            : "text-muted-foreground hover:text-muted-foreground"
        }
        disabled={!canUndo}
        label="Undo"
        tooltip="Undo (Ctrl/Cmd+Z)"
        onClick={onUndo}
      >
        <Undo2 className="text-current" />
      </ToolbarAction>
      <ToolbarAction
        className={
          canRedo
            ? "text-foreground hover:text-foreground"
            : "text-muted-foreground hover:text-muted-foreground"
        }
        disabled={!canRedo}
        label="Redo"
        tooltip="Redo (Shift+Ctrl/Cmd+Z)"
        onClick={onRedo}
      >
        <Redo2 className="text-current" />
      </ToolbarAction>
      {markdownView ? (
        <>
          <span
            aria-orientation="vertical"
            className="mx-1 h-4 w-px bg-border/60"
            role="separator"
          />
          <Tooltip>
            <TooltipTrigger
              render={
                <Toggle
                  aria-label={
                    markdownView.rendered ? "Show markdown source" : "Show rendered markdown"
                  }
                  pressed={markdownView.rendered}
                  size="xs"
                  variant="ghost"
                  onPressedChange={(rendered) => markdownView.onRenderedChange(rendered)}
                >
                  {markdownView.rendered ? (
                    <Code2 className="size-3.5" />
                  ) : (
                    <Eye className="size-3.5" />
                  )}
                </Toggle>
              }
            />
            <TooltipPopup>
              {markdownView.rendered ? "Show markdown source" : "Show rendered markdown"}
            </TooltipPopup>
          </Tooltip>
        </>
      ) : null}
      <span className={statusClassName} aria-live="polite">
        {status}
      </span>
    </div>
  );
}
