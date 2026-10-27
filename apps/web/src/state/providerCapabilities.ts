import { PROVIDER_CAPABILITIES_STALE_TIME_MS } from "@coddy/client-runtime/state/server";
import type { EnvironmentId, ProviderInstanceId } from "@coddy/contracts";
import { useAtomValue } from "@effect/atom-react";
import { useCallback } from "react";

import { useEnvironmentQuery } from "./query";
import { serverEnvironment } from "./server";

export function useProviderCapabilities(target: {
  readonly environmentId: EnvironmentId | null;
  readonly instanceId: ProviderInstanceId | null;
  readonly cwd: string | null;
}) {
  const revision = useAtomValue(serverEnvironment.capabilitiesRevisionAtom(target.environmentId));
  const atom =
    target.environmentId !== null && target.instanceId !== null && target.cwd !== null
      ? serverEnvironment.providerCapabilities({
          environmentId: target.environmentId,
          input: { instanceId: target.instanceId, cwd: target.cwd, revision },
        })
      : null;
  const query = useEnvironmentQuery(atom);
  const { emission, isPending, revalidate } = query;
  const revalidateIfStale = useCallback(() => {
    if (
      emission._tag === "Success" &&
      !isPending &&
      Date.now() - emission.timestamp >= PROVIDER_CAPABILITIES_STALE_TIME_MS
    ) {
      revalidate();
    }
  }, [emission, isPending, revalidate]);
  return { ...query, revalidateIfStale };
}
