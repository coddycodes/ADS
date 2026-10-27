import type { ExecutionEnvironmentDescriptor } from "@coddy/contracts";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";

import { environmentEndpointUrl } from "./endpoint.ts";
import { executeEnvironmentHttpRequest, makeEnvironmentHttpApiClient } from "../rpc/http.ts";

const DEFAULT_REMOTE_REQUEST_TIMEOUT_MS = 10_000;

/** How long a handed-off descriptor stays usable by the next connection attempt. */
export const DESCRIPTOR_HAND_OFF_MAX_AGE_MS = 10_000;

/** A descriptor fetched moments ago, per server, for the next connection attempt only. */
const handedOff = new Map<
  string,
  { readonly descriptor: ExecutionEnvironmentDescriptor; readonly fetchedAtMs: number }
>();

/**
 * Fetches a server's descriptor.
 *
 * `handOff` keeps the result for the next connection attempt to that server, which
 * passes `reuseHandOff` and then takes it instead of fetching it again; the platform
 * fetches a descriptor to register an environment and that environment's first
 * attempt follows at once (one round trip each on a slow link). A hand-off is used
 * once and only within {@link DESCRIPTOR_HAND_OFF_MAX_AGE_MS}, so every later attempt,
 * a reconnect included, still verifies a fresh descriptor.
 */
export const fetchRemoteEnvironmentDescriptor = Effect.fn(
  "clientRuntime.environment.fetchRemoteEnvironmentDescriptor",
)(function* (input: {
  readonly httpBaseUrl: string;
  readonly timeoutMs?: number;
  readonly handOff?: boolean;
  readonly reuseHandOff?: boolean;
}) {
  if (input.reuseHandOff === true) {
    const offered = handedOff.get(input.httpBaseUrl);
    handedOff.delete(input.httpBaseUrl);
    const nowMs = yield* Clock.currentTimeMillis;
    if (offered !== undefined && nowMs - offered.fetchedAtMs <= DESCRIPTOR_HAND_OFF_MAX_AGE_MS) {
      return offered.descriptor;
    }
  }
  const client = yield* makeEnvironmentHttpApiClient(input.httpBaseUrl);
  const descriptor = yield* executeEnvironmentHttpRequest(
    environmentEndpointUrl(input.httpBaseUrl, "/.well-known/coddy-studio/environment"),
    input.timeoutMs ?? DEFAULT_REMOTE_REQUEST_TIMEOUT_MS,
    client.metadata.descriptor(),
  );
  if (input.handOff === true) {
    handedOff.set(input.httpBaseUrl, {
      descriptor,
      fetchedAtMs: yield* Clock.currentTimeMillis,
    });
  }
  return descriptor;
});
