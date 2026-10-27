import {
  createEnvironmentShellAtoms,
  createEnvironmentShellSummaryAtom,
  createEnvironmentSnapshotAtom,
  createConnectionCatalogHealthAtom,
  createShellEnvironmentAtoms,
} from "@coddy/client-runtime/state/shell";
import { useAtomValue } from "@effect/atom-react";

import { environmentCatalog } from "../connection/catalog";
import { connectionAtomRuntime } from "../connection/runtime";

export const shellEnvironment = createShellEnvironmentAtoms(connectionAtomRuntime);
export const environmentShell = createEnvironmentShellAtoms(connectionAtomRuntime);
export const connectionCatalogHealthAtom = createConnectionCatalogHealthAtom(connectionAtomRuntime);
export const environmentSnapshotAtom = createEnvironmentSnapshotAtom(environmentShell.stateAtom);
export const environmentShellSummaryAtom = createEnvironmentShellSummaryAtom({
  catalogValueAtom: environmentCatalog.catalogValueAtom,
  catalogHealthAtom: connectionCatalogHealthAtom,
  shellStateValueAtom: environmentShell.stateValueAtom,
});

export const environmentAvailabilityCommands = {
  retry: environmentCatalog.retryNow,
  adoptStorage: environmentCatalog.acceptStorageIdentity,
} as const;

export function useEnvironmentShellSummary() {
  return useAtomValue(environmentShellSummaryAtom);
}
