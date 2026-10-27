import { RegistryContext, useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@coddy/contracts";
import type { RemoteUpdateRunState } from "@coddy/client-runtime/state/remoteUpdateCoordinator";
import {
  IDLE_REMOTE_UPDATE_CHECK_STATE,
  type RemoteUpdateCheckState,
  createRemoteUpdateEnvironmentAtoms,
} from "@coddy/client-runtime/state/remoteUpdates";
import { runAtomCommand } from "@coddy/client-runtime/state/runtime";
import { Atom, type AtomRegistry } from "effect/reactivity";
import { useCallback, useContext } from "react";

import {
  remoteUpdateFailureMessage,
  remoteUpdateSuccessTitle,
  remoteUpdateUpToDateTitle,
} from "../components/settings/remoteUpdatePresentation";
import { stackedThreadToast, toastManager } from "../components/ui/toast";
import { connectionAtomRuntime } from "../connection/runtime";

export const remoteUpdateEnvironment = createRemoteUpdateEnvironmentAtoms(connectionAtomRuntime);

export interface RemoteUpdateConfirmationRequest {
  readonly environmentId: EnvironmentId;
  readonly name: string;
  readonly targetVersion: string | null;
  /** capabilities.remoteUpdateProgress: the dialog may read activeWork counts. */
  readonly progress: boolean;
}

export const remoteUpdateConfirmationRequest: Atom.Writable<RemoteUpdateConfirmationRequest | null> =
  Atom.make<RemoteUpdateConfirmationRequest | null>(null).pipe(
    Atom.keepAlive,
    Atom.withLabel("web-remote-update:confirmation-request"),
  );

export function requestRemoteUpdateConfirmation(
  registry: AtomRegistry.AtomRegistry,
  request: RemoteUpdateConfirmationRequest,
): void {
  registry.set(remoteUpdateConfirmationRequest, request);
}

export function useRequestRemoteUpdateConfirmation(): (
  request: RemoteUpdateConfirmationRequest,
) => void {
  const registry = useContext(RegistryContext);
  return useCallback(
    (request: RemoteUpdateConfirmationRequest) =>
      requestRemoteUpdateConfirmation(registry, request),
    [registry],
  );
}

/** The run and its outcome toast outlive the view that started them. */
export function startRemoteUpdate(
  registry: AtomRegistry.AtomRegistry,
  request: RemoteUpdateConfirmationRequest,
): void {
  void runAtomCommand(
    registry,
    remoteUpdateEnvironment.update,
    { environmentId: request.environmentId, input: {} },
    { label: remoteUpdateEnvironment.update.label, reportFailure: false },
  ).then((result) => {
    if (result._tag !== "Success") return;
    const outcome = result.value;
    if (outcome.phase === "succeeded") {
      toastManager.add({
        type: "success",
        title: remoteUpdateSuccessTitle(request.name, outcome.version),
      });
    } else if (outcome.phase === "up-to-date") {
      toastManager.add({ type: "info", title: remoteUpdateUpToDateTitle(request.name) });
    } else if (outcome.phase === "failed") {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Update failed",
          description: remoteUpdateFailureMessage(request.name, outcome.failure),
          actionVariant: "outline",
          actionProps: {
            children: "Retry",
            onClick: () => requestRemoteUpdateConfirmation(registry, request),
          },
        }),
      );
    }
  });
}

export function useStartRemoteUpdate(): (request: RemoteUpdateConfirmationRequest) => void {
  const registry = useContext(RegistryContext);
  return useCallback(
    (request: RemoteUpdateConfirmationRequest) => startRemoteUpdate(registry, request),
    [registry],
  );
}

/** Clears settled feedback in the existing runtime owner; never cancels an active install. */
export function useDismissRemoteUpdate(): (environmentId: EnvironmentId) => void {
  const registry = useContext(RegistryContext);
  return useCallback(
    (environmentId: EnvironmentId) => remoteUpdateEnvironment.dismiss(registry, environmentId),
    [registry],
  );
}

const IDLE_CHECK_STATE_ATOM = Atom.make(IDLE_REMOTE_UPDATE_CHECK_STATE).pipe(
  Atom.withLabel("web-remote-update-check:idle"),
);

const NO_RUN_ATOM = Atom.make<RemoteUpdateRunState | null>(null).pipe(
  Atom.withLabel("web-remote-update-run:none"),
);

/** Update-check progress and failure for one environment; idle without one. */
export function useRemoteUpdateCheckState(
  environmentId: EnvironmentId | null,
): RemoteUpdateCheckState {
  return useAtomValue(
    environmentId === null
      ? IDLE_CHECK_STATE_ATOM
      : remoteUpdateEnvironment.checkState(environmentId),
  );
}

/** The environment's update run, whichever view started it; null without one. */
export function useRemoteUpdateRun(
  environmentId: EnvironmentId | null,
): RemoteUpdateRunState | null {
  return useAtomValue(
    environmentId === null ? NO_RUN_ATOM : remoteUpdateEnvironment.run(environmentId),
  );
}
