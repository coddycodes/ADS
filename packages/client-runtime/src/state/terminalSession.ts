import type {
  EnvironmentId,
  TerminalMetadataStreamEvent,
  TerminalConsoleTheme,
  TerminalSessionSnapshot,
  TerminalSummary,
  ThreadId,
} from "@coddy/contracts";

import type { TerminalMetadataSnapshot } from "./terminalTranscriptRuntime.ts";

export { DEFAULT_MAX_TERMINAL_BUFFER_BYTES } from "./terminalTranscript.ts";

export interface TerminalSessionState {
  readonly summary: TerminalSummary | null;
  readonly status: TerminalSessionSnapshot["status"] | "closed";
  readonly error: string | null;
  readonly consoleTheme: TerminalConsoleTheme | null;
  readonly hasRunningSubprocess: boolean;
  readonly updatedAt: string | null;
  readonly generation: number;
}

export interface KnownTerminalSessionTarget {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly terminalId: string;
}

export interface KnownTerminalSession {
  readonly target: KnownTerminalSessionTarget;
  readonly state: TerminalSessionState;
}

export function selectRunningSubprocessTerminalIds(
  sessions: ReadonlyArray<KnownTerminalSession>,
): ReadonlyArray<string> {
  return sessions
    .filter((session) => session.state.hasRunningSubprocess)
    .map((session) => session.target.terminalId);
}

export const EMPTY_TERMINAL_SESSION_STATE = Object.freeze<TerminalSessionState>({
  summary: null,
  status: "closed",
  error: null,
  consoleTheme: null,
  hasRunningSubprocess: false,
  updatedAt: null,
  generation: 0,
});

export function combineTerminalSessionState(
  summary: TerminalSummary | null,
  metadata: TerminalMetadataSnapshot,
): TerminalSessionState {
  const attachHasEmitted = metadata.revision > 0;
  return {
    summary,
    status: attachHasEmitted ? metadata.status : (summary?.status ?? metadata.status),
    error: metadata.error,
    consoleTheme: metadata.consoleTheme ?? summary?.consoleTheme ?? null,
    hasRunningSubprocess: summary?.hasRunningSubprocess ?? false,
    updatedAt: summary?.updatedAt ?? null,
    generation: metadata.generation,
  };
}

export function applyTerminalMetadataStreamEvent(
  current: ReadonlyArray<TerminalSummary>,
  event: TerminalMetadataStreamEvent,
): ReadonlyArray<TerminalSummary> {
  if (event.type === "snapshot") {
    return event.terminals;
  }
  if (event.type === "remove") {
    return current.filter(
      (terminal) =>
        terminal.threadId !== event.threadId || terminal.terminalId !== event.terminalId,
    );
  }
  const next = current.filter(
    (terminal) =>
      terminal.threadId !== event.terminal.threadId ||
      terminal.terminalId !== event.terminal.terminalId,
  );
  return [...next, event.terminal];
}
