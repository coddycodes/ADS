import { isPersistedConnectionTarget } from "@coddy/client-runtime/connection";

import { appAtomRegistry } from "~/rpc/atomRegistry";

import { environmentCatalog } from "./catalog";

const SAVED_ENVIRONMENTS_WAIT_MS = 10_000;

/**
 * Whether this browser saved any environment of its own (a paired remote server, a relay, an SSH
 * host), as opposed to the server it was served by. `null` when the catalog did not load in time:
 * it may still hold a server just paired, so a caller must not treat that as none.
 */
export function hasSavedEnvironments(): Promise<boolean | null> {
  return new Promise((resolve) => {
    let settled = false;
    let unsubscribe: (() => void) | null = null;
    const settle = (value: boolean | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      queueMicrotask(() => unsubscribe?.());
      resolve(value);
    };
    const timer = setTimeout(() => settle(null), SAVED_ENVIRONMENTS_WAIT_MS);
    unsubscribe = appAtomRegistry.subscribe(
      environmentCatalog.catalogValueAtom,
      (catalog) => {
        if (!catalog.isReady) return;
        settle(
          [...catalog.entries.values()].some((entry) => isPersistedConnectionTarget(entry.target)),
        );
      },
      { immediate: true },
    );
    if (settled) queueMicrotask(() => unsubscribe?.());
  });
}
