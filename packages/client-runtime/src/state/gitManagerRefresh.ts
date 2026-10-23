import type { GitManagerSignalEvent } from "@bibcode/contracts";
import { AsyncResult, Atom } from "effect/reactivity";

/** Explicitly retains the platform wakeup subscription for this repository's fallback. */
export function createSignalWithDegradedFocusRefresh<E, EW>(
  signal: Atom.Atom<AsyncResult.AsyncResult<GitManagerSignalEvent, E>>,
  focusVisibility: Atom.Atom<AsyncResult.AsyncResult<number, EW>>,
  focusRefresh: Atom.Writable<number>,
) {
  return Atom.make((get) => {
    get.mount(focusRefresh);
    get.once(focusVisibility);
    get.subscribe(focusVisibility, (wakeup) => {
      const current = get.once(signal);
      if (
        AsyncResult.isSuccess(wakeup) &&
        AsyncResult.isSuccess(current) &&
        current.value.watcherDegraded === true
      ) {
        get.set(focusRefresh, get.once(focusRefresh) + 1);
      }
    });
    return get(signal);
  }).pipe(Atom.setIdleTTL(0));
}
