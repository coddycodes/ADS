import { computeCompatVerdict, type CompatVerdict } from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";

/**
 * Compatibility verdict for an environment. `null` means the environment has
 * never delivered a descriptor, so callers should degrade without treating it
 * as a legacy server.
 */
export function resolveEnvironmentCompatVerdict(
  serverConfig: ServerConfig | null,
): CompatVerdict | null {
  if (serverConfig === null) {
    return null;
  }
  return computeCompatVerdict(serverConfig.environment);
}

export function selectRemoteUpdateControlCapability(serverConfig: ServerConfig | null): boolean {
  return serverConfig?.environment.capabilities.remoteUpdateControl === true;
}
