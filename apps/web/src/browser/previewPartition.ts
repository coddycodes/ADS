import type { EnvironmentId } from "@bibcode/contracts";

import { appAtomRegistry } from "~/rpc/atomRegistry";
import { primaryEnvironmentIdAtom } from "~/state/primaryEnvironment";

/**
 * The preview storage partition for an environment's native preview tabs:
 * `null` keeps the local environment on the original shared profile; every
 * other environment gets its own, so previews of the same `localhost` origin
 * from different environments never share cookies, storage, or service workers.
 */
export function previewPartitionFor(environmentId: EnvironmentId): string | null {
  return appAtomRegistry.get(primaryEnvironmentIdAtom) === environmentId ? null : environmentId;
}
