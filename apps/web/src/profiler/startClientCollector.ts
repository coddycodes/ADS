import type { EnvironmentPresentation } from "@bibcode/client-runtime/connection";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import { EnvironmentId } from "@bibcode/contracts";
import type { AtomRegistry } from "effect/unstable/reactivity";

import { environmentPresentations } from "../state/presentation";
import { serverEnvironment } from "../state/server";
import { clientCollector, setClientBatchSender } from "./clientCollector";

/** The connected environments whose `enableProfiler` setting is on. */
function profiledEnvironments(
  presentations: ReadonlyMap<string, EnvironmentPresentation>,
): ReadonlySet<string> {
  const ids = new Set<string>();
  for (const [environmentId, presentation] of presentations) {
    if (
      presentation.connection.phase === "connected" &&
      presentation.serverConfig?.settings.enableProfiler === true
    ) {
      ids.add(environmentId);
    }
  }
  return ids;
}

/**
 * Connects the app collector to the environments' settings and to the batch RPC. Lives here
 * rather than in `clientCollector.ts` because the RPC observer in `connection/platform.ts`
 * imports the collector, and `state/server` imports the platform layer.
 */
export function startClientCollector(registry: AtomRegistry.AtomRegistry): () => void {
  setClientBatchSender(async (environmentId, batch) => {
    const result = await serverEnvironment.reportProfilerClientBatch.run(registry, {
      environmentId: EnvironmentId.make(environmentId),
      input: batch,
    });
    // Rejected so the collector can see an authorization refusal; it swallows everything.
    if (result._tag === "Failure") throw squashAtomCommandFailure(result);
  });
  const unsubscribe = registry.subscribe(
    environmentPresentations.presentationsAtom,
    (presentations) => clientCollector.setEnabledEnvironments(profiledEnvironments(presentations)),
    { immediate: true },
  );
  return () => {
    unsubscribe();
    clientCollector.setEnabledEnvironments(new Set());
    setClientBatchSender(null);
  };
}
