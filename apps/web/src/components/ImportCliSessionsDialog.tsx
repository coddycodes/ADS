import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";
import type {
  AgentSessionCandidate,
  AgentSessionImportResult,
  AgentSessionProvider,
  EnvironmentId,
  ProjectId,
  ThreadId,
} from "@coddy/contracts";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import { projectEnvironment } from "../state/projects";
import { useAtomCommand } from "../state/use-atom-command";
import { formatRelativeTimeLabel } from "../timestampFormat";
import { ClaudeAI, type Icon, OpenAI } from "./Icons";
import { Button } from "./ui/button";
import { Checkbox } from "./ui/checkbox";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";

export interface ImportCliSessionsTarget {
  readonly environmentId: EnvironmentId;
  readonly projectId: ProjectId;
  readonly workspaceRoot: string;
}

export interface ImportCliSessionsSummary {
  readonly type: "success" | "warning" | "error";
  readonly title: string;
  readonly description?: string;
  /** The imported thread whose session was active most recently. */
  readonly newestThreadId: ThreadId | null;
}

export interface ImportCliSessionsDialogProps {
  readonly open: boolean;
  readonly target: ImportCliSessionsTarget | null;
  readonly onOpenChange: (open: boolean) => void;
  readonly onImported: (target: ImportCliSessionsTarget, summary: ImportCliSessionsSummary) => void;
  readonly onOpenThread: (target: ImportCliSessionsTarget, threadId: ThreadId) => void;
}

const PROVIDERS: Record<AgentSessionProvider, { readonly label: string; readonly icon: Icon }> = {
  claudeAgent: { label: "Claude Code", icon: ClaudeAI },
  codex: { label: "Codex", icon: OpenAI },
};

const NOTHING_SELECTED_REASON = "Select the sessions to import.";

function plural(count: number, singular: string): string {
  return `${count} ${singular}${count === 1 ? "" : "s"}`;
}

function failureMessage(result: { readonly cause: unknown }, fallback: string): string {
  const error = squashAtomCommandFailure(result as never);
  return error instanceof Error && error.message.trim().length > 0 ? error.message : fallback;
}

/** The toast an import ends with, and the thread to open. */
export function summarizeImport(
  result: AgentSessionImportResult,
  candidates: ReadonlyArray<AgentSessionCandidate>,
): ImportCliSessionsSummary {
  const lastActiveAt = new Map(
    candidates.map((candidate) => [candidate.sessionId, candidate.lastActiveAt]),
  );
  const newest = result.imported.reduce<(typeof result.imported)[number] | null>(
    (best, entry) =>
      best === null ||
      (lastActiveAt.get(entry.sessionId) ?? "") > (lastActiveAt.get(best.sessionId) ?? "")
        ? entry
        : best,
    null,
  );
  const reasons = [...new Set(result.skipped.map((entry) => entry.reason))];
  const skipped =
    result.skipped.length === 0
      ? undefined
      : `${plural(result.skipped.length, "session")} skipped: ${reasons.join(" ")}`;
  if (result.imported.length === 0) {
    return {
      type: "error",
      title: "No sessions imported",
      ...(skipped === undefined ? {} : { description: skipped }),
      newestThreadId: null,
    };
  }
  return {
    type: skipped === undefined ? "success" : "warning",
    title: `Imported ${plural(result.imported.length, "session")}`,
    ...(skipped === undefined ? {} : { description: skipped }),
    newestThreadId: newest?.threadId ?? null,
  };
}

const candidateKey = (candidate: AgentSessionCandidate) =>
  `${candidate.provider}:${candidate.sessionId}`;

/** An earlier import: its live thread opens; a deleted one cannot be imported again. */
function ImportedMarker({
  threadId,
  disabled,
  onOpen,
}: {
  readonly threadId: ThreadId | undefined;
  readonly disabled: boolean;
  readonly onOpen: (threadId: ThreadId) => void;
}) {
  if (threadId === undefined) {
    return <span className="shrink-0 text-xs text-muted-foreground">Imported, thread deleted</span>;
  }
  return (
    <>
      <span className="shrink-0 text-xs text-muted-foreground">Already imported</span>
      <Button
        type="button"
        size="xs"
        variant="outline"
        disabled={disabled}
        onClick={() => onOpen(threadId)}
      >
        Open
      </Button>
    </>
  );
}

/**
 * Each opening mounts fresh state and scans again, so sessions imported or run since the
 * last visit show as they are.
 */
export function ImportCliSessionsDialog(props: ImportCliSessionsDialogProps) {
  const { open, target } = props;
  const key = open && target ? `${target.environmentId}:${target.projectId}` : "closed";
  return <ImportCliSessionsDialogContent key={key} {...props} />;
}

function ImportCliSessionsDialogContent({
  open,
  target,
  onOpenChange,
  onImported,
  onOpenThread,
}: ImportCliSessionsDialogProps) {
  const scanSessions = useAtomCommand(projectEnvironment.scanAgentSessions, {
    reportFailure: false,
  });
  const importSessions = useAtomCommand(projectEnvironment.importAgentSessions, {
    reportFailure: false,
  });
  const [candidates, setCandidates] = useState<ReadonlyArray<AgentSessionCandidate> | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  const [isScanning, setIsScanning] = useState(() => open && target !== null);
  const [isImporting, setIsImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nothingSelectedId = useId();

  const applyScan = useCallback((result: Awaited<ReturnType<typeof scanSessions>>) => {
    setIsScanning(false);
    if (result._tag === "Failure") {
      if (!isAtomCommandInterrupted(result)) {
        setError(failureMessage(result, "The server could not list CLI sessions."));
      }
      return;
    }
    setCandidates(result.value.candidates);
    setTruncated(result.value.truncated);
  }, []);

  // The server reads every matching transcript; a scan nobody will see is aborted.
  const scanController = useRef<AbortController | null>(null);
  const scan = useCallback(
    (requestTarget: ImportCliSessionsTarget) => {
      scanController.current?.abort();
      const controller = new AbortController();
      scanController.current = controller;
      return scanSessions(
        {
          environmentId: requestTarget.environmentId,
          input: { projectId: requestTarget.projectId },
        },
        { signal: controller.signal },
      ).then((result) => {
        if (!controller.signal.aborted) applyScan(result);
      });
    },
    [applyScan, scanSessions],
  );

  // Aborts whichever scan is current, including one started by Try again.
  const abortScan = useCallback(() => scanController.current?.abort(), []);

  useEffect(() => {
    if (!open || !target) return;
    void scan(target);
    return abortScan;
  }, [abortScan, open, scan, target]);

  const retry = useCallback(() => {
    if (!target) return;
    setIsScanning(true);
    setError(null);
    void scan(target);
  }, [scan, target]);

  const importable = useMemo(
    () => (candidates ?? []).filter((candidate) => !candidate.alreadyImported),
    [candidates],
  );
  const selectedCount = importable.filter((candidate) =>
    selected.has(candidateKey(candidate)),
  ).length;
  const allSelected = importable.length > 0 && selectedCount === importable.length;

  const toggle = useCallback((key: string, checked: boolean) => {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(key);
      else next.delete(key);
      return next;
    });
  }, []);

  const toggleAll = useCallback(
    (checked: boolean) => {
      setSelected(checked ? new Set(importable.map(candidateKey)) : new Set());
    },
    [importable],
  );

  const runImport = useCallback(async () => {
    if (!target || !candidates || isImporting) return;
    const sessions = importable
      .filter((candidate) => selected.has(candidateKey(candidate)))
      .map(({ provider, sessionId }) => ({ provider, sessionId }));
    if (sessions.length === 0) return;
    const requestTarget = target;
    setIsImporting(true);
    setError(null);
    const result = await importSessions({
      environmentId: requestTarget.environmentId,
      input: { projectId: requestTarget.projectId, sessions },
    });
    setIsImporting(false);
    if (result._tag === "Failure") {
      if (!isAtomCommandInterrupted(result)) {
        setError(failureMessage(result, "The import failed. Try again."));
      }
      return;
    }
    onImported(requestTarget, summarizeImport(result.value, candidates));
  }, [candidates, importSessions, importable, isImporting, onImported, selected, target]);

  const openThread = useCallback(
    (threadId: ThreadId) => {
      if (target) onOpenThread(target, threadId);
    },
    [onOpenThread, target],
  );

  const handleOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (!nextOpen && isImporting) return;
      onOpenChange(nextOpen);
    },
    [isImporting, onOpenChange],
  );

  const workspaceRoot = target?.workspaceRoot ?? "this project";

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogPopup className="max-w-xl" showCloseButton={!isImporting}>
        <DialogHeader>
          <DialogTitle>Import CLI sessions</DialogTitle>
          <DialogDescription>
            Claude Code and Codex sessions run in {workspaceRoot} on the server. An imported thread
            continues its CLI conversation.
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-3">
          {isScanning ? (
            <p role="status" className="text-sm text-muted-foreground">
              Looking for sessions…
            </p>
          ) : null}
          {error ? (
            <div role="alert" className="space-y-2 text-sm text-destructive">
              <p>{error}</p>
              {candidates === null ? (
                <Button type="button" size="sm" variant="outline" onClick={retry}>
                  Try again
                </Button>
              ) : null}
            </div>
          ) : null}
          {candidates !== null && candidates.length === 0 ? (
            <p role="status" className="text-sm text-muted-foreground">
              No Claude Code or Codex sessions found for {workspaceRoot} in the last 30 days.
            </p>
          ) : null}
          {candidates !== null && candidates.length > 0 ? (
            <>
              <label className="flex items-center gap-3 px-2 text-sm font-medium">
                <Checkbox
                  checked={allSelected}
                  indeterminate={selectedCount > 0 && !allSelected}
                  disabled={importable.length === 0 || isImporting}
                  onCheckedChange={toggleAll}
                />
                Select all ({importable.length})
              </label>
              <ul
                data-text-surface
                aria-label="CLI sessions"
                className="max-h-[50vh] space-y-0.5 overflow-y-auto rounded-md border p-1"
              >
                {candidates.map((candidate) => {
                  const key = candidateKey(candidate);
                  const provider = PROVIDERS[candidate.provider];
                  const ProviderIcon = provider.icon;
                  return (
                    <li key={key} className="flex items-center gap-2 rounded-md px-2 py-1.5">
                      <label className="flex min-w-0 flex-1 cursor-pointer items-start gap-3">
                        <Checkbox
                          className="mt-0.5"
                          checked={candidate.alreadyImported || selected.has(key)}
                          disabled={candidate.alreadyImported || isImporting}
                          onCheckedChange={(checked) => toggle(key, checked)}
                        />
                        <ProviderIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
                        <span className="min-w-0 flex-1">
                          <span
                            className="block truncate text-sm text-foreground"
                            title={candidate.title}
                          >
                            {candidate.title}
                          </span>
                          <span className="block text-xs text-muted-foreground">
                            {provider.label} · {formatRelativeTimeLabel(candidate.lastActiveAt)} ·{" "}
                            {plural(candidate.messageCount, "message")}
                          </span>
                        </span>
                      </label>
                      {candidate.alreadyImported ? (
                        <ImportedMarker
                          threadId={candidate.threadId}
                          disabled={isImporting}
                          onOpen={openThread}
                        />
                      ) : null}
                    </li>
                  );
                })}
              </ul>
              {truncated ? (
                <p className="text-xs text-muted-foreground">
                  Showing the {candidates.length} most recent sessions.
                </p>
              ) : null}
            </>
          ) : null}
          {candidates !== null && importable.length > 0 && selectedCount === 0 ? (
            <p id={nothingSelectedId} className="text-xs text-muted-foreground">
              {NOTHING_SELECTED_REASON}
            </p>
          ) : null}
        </DialogPanel>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={isImporting}
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          {importable.length > 0 ? (
            <Button
              type="button"
              disabled={selectedCount === 0 || isImporting}
              aria-describedby={selectedCount === 0 ? nothingSelectedId : undefined}
              onClick={() => void runImport()}
            >
              {isImporting
                ? "Importing…"
                : selectedCount === 0
                  ? "Import"
                  : `Import ${plural(selectedCount, "session")}`}
            </Button>
          ) : null}
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
