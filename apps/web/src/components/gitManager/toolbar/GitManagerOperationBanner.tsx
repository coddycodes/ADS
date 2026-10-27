import type { GitManagerOperationEvent } from "@coddy/contracts";
import { ChevronDownIcon, ChevronRightIcon, LoaderCircleIcon, XIcon } from "lucide-react";
import { memo, useEffect, useReducer } from "react";

import { Button } from "~/components/ui/button";

interface OutputChunk {
  readonly id: number;
  readonly stream: "stdout" | "stderr";
  readonly text: string;
}

interface OperationBannerState {
  readonly visible: boolean;
  readonly status: "running" | "failed";
  readonly operation: string;
  readonly chunks: ReadonlyArray<OutputChunk>;
  readonly expanded: boolean;
  readonly failureCode: string | null;
  readonly failureMessage: string | null;
}

const CLOSED_OPERATION_STATE: OperationBannerState = Object.freeze({
  visible: false,
  status: "running",
  operation: "",
  chunks: Object.freeze([]),
  expanded: false,
  failureCode: null,
  failureMessage: null,
});

type BannerAction =
  | { readonly kind: "event"; readonly event: GitManagerOperationEvent }
  | { readonly kind: "toggle-output" };

function reduceBannerState(
  state: OperationBannerState,
  action: BannerAction,
): OperationBannerState {
  if (action.kind === "toggle-output") {
    return { ...state, expanded: !state.expanded };
  }

  const event = action.event;
  switch (event._tag) {
    case "started":
      return {
        visible: true,
        status: "running",
        operation: event.operation,
        chunks: [],
        expanded: false,
        failureCode: null,
        failureMessage: null,
      };
    case "output":
      return {
        ...state,
        visible: true,
        status: "running",
        operation: event.operation,
        chunks: [
          ...state.chunks,
          { id: state.chunks.length, stream: event.stream, text: event.text },
        ],
      };
    case "finished":
      return CLOSED_OPERATION_STATE;
    case "failed":
      return {
        ...state,
        visible: true,
        status: "failed",
        operation: event.operation,
        failureCode: event.code,
        failureMessage: event.blocked?.message ?? event.message,
      };
  }
}

export interface GitManagerOperationBannerProps {
  readonly operation: GitManagerOperationEvent | null;
  readonly onCancel: () => void;
}

export const GitManagerOperationBanner = memo(function GitManagerOperationBanner({
  operation,
  onCancel,
}: GitManagerOperationBannerProps) {
  const [state, dispatch] = useReducer(reduceBannerState, CLOSED_OPERATION_STATE);
  // Each new event object is applied once, even when it equals the previous one.
  useEffect(() => {
    if (operation === null) return;
    dispatch({ kind: "event", event: operation });
  }, [operation]);

  if (!state.visible) return null;
  const outputId = "git-manager-operation-output";

  return (
    <section
      aria-atomic="true"
      aria-live="polite"
      className="border-b border-border bg-card/70 px-3 py-2 text-xs"
      role="status"
    >
      <div className="flex min-w-0 items-center gap-2">
        {state.status === "running" ? (
          <LoaderCircleIcon aria-hidden="true" className="size-4 shrink-0 animate-spin" />
        ) : (
          <XIcon aria-hidden="true" className="size-4 shrink-0 text-destructive" />
        )}
        <span className="min-w-0 flex-1 truncate font-medium">
          {state.status === "running" ? `Running ${state.operation}…` : `${state.operation} failed`}
        </span>
        {state.chunks.length === 0 ? null : (
          <Button
            aria-controls={outputId}
            aria-expanded={state.expanded}
            size="xs"
            variant="ghost"
            onClick={() => dispatch({ kind: "toggle-output" })}
          >
            {state.expanded ? (
              <ChevronDownIcon aria-hidden="true" />
            ) : (
              <ChevronRightIcon aria-hidden="true" />
            )}
            Output ({state.chunks.length})
          </Button>
        )}
        {state.status === "running" ? (
          <Button size="xs" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
      </div>
      {state.failureMessage === null ? null : (
        <p className="mt-1 text-destructive">
          {state.failureMessage} <code className="font-mono">({state.failureCode})</code>
        </p>
      )}
      {state.chunks.length === 0 ? null : (
        <div data-operation-output hidden={!state.expanded} id={outputId}>
          <p className="mt-2 text-xs text-muted-foreground">
            Output arrives in chunks after each Git command completes.
          </p>
          <div className="mt-1 max-h-40 overflow-auto rounded bg-background p-2 font-mono text-xs">
            {state.chunks.map((chunk) => (
              <pre
                className={chunk.stream === "stderr" ? "text-destructive" : undefined}
                key={chunk.id}
              >
                {chunk.text}
              </pre>
            ))}
          </div>
        </div>
      )}
    </section>
  );
});
