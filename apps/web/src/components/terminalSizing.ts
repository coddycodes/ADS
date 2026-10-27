import type { FitAddon } from "@xterm/addon-fit";
import {
  TERMINAL_COLS_MIN,
  TERMINAL_COLS_MAX,
  TERMINAL_ROWS_MIN,
  TERMINAL_ROWS_MAX,
} from "@coddy/contracts";
import { terminalInputKey } from "@coddy/client-runtime/state/terminal";
import type { TerminalDimensions } from "./terminalSizePolicy";

export function proposeTerminalDimensions(
  fit: Pick<FitAddon, "proposeDimensions">,
): TerminalDimensions | null {
  try {
    const size = fit.proposeDimensions();
    return size && Number.isFinite(size.cols) && Number.isFinite(size.rows)
      ? {
          cols: Math.min(TERMINAL_COLS_MAX, Math.max(TERMINAL_COLS_MIN, size.cols)),
          rows: Math.min(TERMINAL_ROWS_MAX, Math.max(TERMINAL_ROWS_MIN, size.rows)),
        }
      : null;
  } catch {
    return null;
  }
}

// Only mounted renderers register a reader. Launch callbacks can use their
// current fit without subscribing to layout or manufacturing a measuring xterm.
const sizeReaders = new Map<string, () => TerminalDimensions | null>();

export function registerTerminalSizeReader(
  key: string,
  reader: () => TerminalDimensions | null,
): () => void {
  sizeReaders.set(key, reader);
  return () => {
    if (sizeReaders.get(key) === reader) sizeReaders.delete(key);
  };
}

export function readTerminalFittedSize(
  environmentId: string,
  threadId: string,
  terminalId: string,
): TerminalDimensions | null {
  return sizeReaders.get(terminalInputKey(environmentId, threadId, terminalId))?.() ?? null;
}
