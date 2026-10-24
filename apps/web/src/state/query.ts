import {
  isEnvironmentQueryAwaitingRetry,
  isQueryTransportCutoff,
  retryEnvironmentQuery,
} from "@bibcode/client-runtime/state/runtime";
import { useAtomRefresh, useAtomValue } from "@effect/atom-react";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/reactivity";
import { useCallback } from "react";

const EMPTY_ASYNC_RESULT_ATOM = Atom.make(AsyncResult.initial<never, never>(false)).pipe(
  Atom.withLabel("web-environment-query:empty"),
);

export interface EnvironmentQueryView<A, E = unknown> {
  readonly data: A | null;
  /** The exact AsyncResult emission observed by this render. */
  readonly emission: AsyncResult.AsyncResult<A, E>;
  readonly error: string | null;
  /** Stream queries keep waiting between emissions; initial loading also requires data === null. */
  readonly isPending: boolean;
  /** Explicit user Retry: also clears an exhausted transport cut-off. */
  readonly refresh: () => void;
  /** Automatic re-read (timers, signals, mutations, opening a view); keeps cut-off state. */
  readonly revalidate: () => void;
  /** The automatic re-issue after a cut-off failed; only `refresh` sends the request again. */
  readonly requiresRetry: boolean;
}

/** Shown when the connection dropped mid-request; the view's Retry reloads it. */
export const QUERY_CONNECTION_DROPPED_MESSAGE = "The connection dropped before the result arrived.";

/**
 * The one message rule for a failed environment query: a transport cut-off, or an
 * attempt interrupted by its closing session, gets the connection-dropped copy; an
 * error with a non-blank message shows it; anything else shows `fallback`. Views pass
 * a fallback that names what failed; the default is generic.
 */
export function formatEnvironmentQueryError(
  cause: Cause.Cause<unknown>,
  fallback = "The environment request failed.",
): string {
  // The RPC client interrupts a closing session's requests; squashing that cause would
  // show Effect's "All fibers interrupted without error".
  if (Cause.hasInterruptsOnly(cause)) return QUERY_CONNECTION_DROPPED_MESSAGE;
  const error = Cause.squash(cause);
  if (isQueryTransportCutoff(error)) return QUERY_CONNECTION_DROPPED_MESSAGE;
  return error instanceof Error && error.message.trim().length > 0 ? error.message : fallback;
}

export function useEnvironmentQuery<A, E>(
  atom: Atom.Atom<AsyncResult.AsyncResult<A, E>> | null,
): EnvironmentQueryView<A, E> {
  const selectedAtom = atom ?? EMPTY_ASYNC_RESULT_ATOM;
  const result = useAtomValue(selectedAtom);
  const refreshAtom = useAtomRefresh(selectedAtom);
  const refresh = useCallback(
    () => retryEnvironmentQuery(selectedAtom, refreshAtom),
    [selectedAtom, refreshAtom],
  );
  return {
    data: Option.getOrNull(AsyncResult.value(result)),
    emission: result,
    error: result._tag === "Failure" ? formatEnvironmentQueryError(result.cause) : null,
    isPending: atom !== null && result.waiting,
    refresh,
    revalidate: refreshAtom,
    requiresRetry: isEnvironmentQueryAwaitingRetry(selectedAtom, result),
  };
}
