import type { ProjectId, ResolvedKeybindingsConfig, ScopedThreadRef } from "@coddy/contracts";
import { resolveTerminalSessionLabel } from "@coddy/shared/terminalLabels";
import { useMemo } from "react";

import type { TerminalContextSelection } from "~/lib/terminalContext";
import type { CenterSurface } from "~/centerPanelStore";
import { useKnownTerminalSessions } from "~/state/terminalSessions";

import ThreadTerminalPanel from "./ThreadTerminalPanel";

interface CenterTerminalPanelProps {
  /** The HOST thread ref — center terminals reuse the host thread's attach layer. */
  threadRef: ScopedThreadRef;
  projectId: ProjectId;
  surface: Extract<CenterSurface, { kind: "terminal" }>;
  launchContext: {
    readonly cwd: string;
    readonly worktreePath: string | null;
    readonly runtimeEnv: Record<string, string>;
  } | null;
  keybindings: ResolvedKeybindingsConfig;
  focusRequestId: number;
  focusEligible: boolean;
  onAddTerminalContext: (selection: TerminalContextSelection) => void;
  /** Invoked when the terminal is closed from within its own chrome. */
  onClose: () => void;
  workspaceUnavailable?: string | null;
}

const noop = () => undefined;

/**
 * Thin center-area host for a single terminal, tied to the host thread.
 *
 * The xterm view + attach layer are owned by `ThreadTerminalPanel`. This resolves
 * cwd/worktree/runtimeEnv from the host thread's project — the same derivation
 * as ChatView's PersistentThreadTerminalPanel, which is kept private to that
 * file to avoid a ChatView↔CenterTerminalPanel import cycle. This component
 * owns one center terminal surface. Center workspace grouping is external;
 * terminal-process split controls remain intentionally omitted.
 */
export function CenterTerminalPanel({
  threadRef,
  projectId,
  surface,
  launchContext,
  keybindings,
  focusRequestId,
  focusEligible,
  onAddTerminalContext,
  onClose,
  workspaceUnavailable = null,
}: CenterTerminalPanelProps) {
  const { terminalId, command, label } = surface;
  const knownTerminalSessions = useKnownTerminalSessions({
    environmentId: threadRef.environmentId,
    threadId: threadRef.threadId,
  });
  const activeSummary =
    knownTerminalSessions.find((session) => session.target.terminalId === terminalId)?.state
      .summary ?? null;
  const terminalCommandsById = useMemo(
    () => (command ? new Map([[terminalId, command]]) : undefined),
    [command, terminalId],
  );
  const terminalLabelsById = useMemo(
    () => new Map([[terminalId, label ?? resolveTerminalSessionLabel(terminalId, activeSummary)]]),
    [activeSummary, label, terminalId],
  );

  if (launchContext === null) return null;

  return (
    <ThreadTerminalPanel
      owner="center-panel"
      threadRef={threadRef}
      threadId={threadRef.threadId}
      projectId={projectId}
      cwd={launchContext.cwd}
      worktreePath={launchContext.worktreePath}
      runtimeEnv={launchContext.runtimeEnv}
      terminalIds={[terminalId]}
      activeTerminalId={terminalId}
      terminalGroups={[{ id: `terminal:${terminalId}`, terminalIds: [terminalId] }]}
      activeTerminalGroupId={`terminal:${terminalId}`}
      focusRequestId={focusRequestId}
      focusEligible={focusEligible}
      onActiveTerminalChange={noop}
      onCloseTerminal={onClose}
      onAddTerminalContext={onAddTerminalContext}
      terminalLabelsById={terminalLabelsById}
      {...(terminalCommandsById ? { terminalCommandsById } : {})}
      keybindings={keybindings}
      workspaceUnavailable={workspaceUnavailable}
    />
  );
}
