import { EnvironmentId } from "@coddy/contracts";

import type { BearerConnectionTarget } from "./model.ts";

/**
 * Every Coddy Studio server declares the same environment id (`"local"`), so a
 * host's own claim about itself cannot key the client's environment registry:
 * the desktop app's own Local environment and every remote host would fight
 * over one key. A saved remote is keyed by its storage instance id instead —
 * a UUID minted per data root — under this namespace, alongside the existing
 * `ssh:`, `wsl:`, and `desktop-local:` conventions.
 */
export const REMOTE_ENVIRONMENT_ID_PREFIX = "remote:";

export function remoteEnvironmentId(storageInstanceId: string): EnvironmentId {
  return EnvironmentId.make(`${REMOTE_ENVIRONMENT_ID_PREFIX}${storageInstanceId}`);
}

/**
 * The environment id the host declares about itself, which the resolver
 * re-checks on every connect. Entries saved before remote ids were derived
 * carry that value in `environmentId`, so the fallback is exact for them.
 */
export function bearerServerEnvironmentId(target: BearerConnectionTarget): EnvironmentId {
  return target.serverEnvironmentId ?? target.environmentId;
}
