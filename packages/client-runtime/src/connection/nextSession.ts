import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClientError } from "effect/rpc";

import { EnvironmentRpcUnavailableError } from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";
import { EnvironmentRegistry } from "./registry.ts";
import { EnvironmentSupervisor } from "./supervisor.ts";

const isRpcClientError = Schema.is(RpcClientError.RpcClientError);
const isEnvironmentRpcUnavailableError = Schema.is(EnvironmentRpcUnavailableError);

/**
 * The connection went away before the server answered: an RPC transport error, no session,
 * or the interrupt Effect's RPC client resumes a pending call with when its socket closes.
 * A server answer (any typed failure) is never a transport loss.
 */
export function isSessionTransportLoss(cause: Cause.Cause<unknown>): boolean {
  return (
    cause.reasons.length > 0 &&
    cause.reasons.every(
      (reason) =>
        Cause.isInterruptReason(reason) ||
        (Cause.isFailReason(reason) &&
          (isRpcClientError(reason.error) || isEnvironmentRpcUnavailableError(reason.error))),
    )
  );
}

export type NextSession =
  | {
      readonly _tag: "Session";
      readonly session: RpcSession;
      /** The supervisor that owns `session`, which the caller then treats as known. */
      readonly supervisor: EnvironmentSupervisor["Service"];
    }
  | { readonly _tag: "Stopped"; readonly label: string };

/**
 * Waits, as `subscribe()` does, for a session other than `lost` on the environment's current
 * supervisor, followed through the registry. The registry replaces a supervisor (a changed
 * platform registration, for example) by retiring it without ever reporting a stop, so the wait
 * moves to the replacement and takes its sessions. Ends as `Stopped` when the environment is
 * blocked, disconnected by the user, or removed from the registry. No attempt limit and no timer.
 *
 * `known` is the supervisor the caller last used; it names the environment. Every supervisor,
 * a replacement included, publishes the registry's connection intent from its first state on, so
 * an undesired state on the current supervisor means the user disconnected.
 *
 * A stop names the environment by its current label: the current supervisor's or, once the
 * environment is removed, the last label the registry published for it.
 */
export function nextSession(
  registry: EnvironmentRegistry["Service"],
  known: EnvironmentSupervisor["Service"],
  lost: RpcSession | null,
): Effect.Effect<NextSession> {
  const environmentId = known.target.environmentId;
  let label = known.target.label;
  const onCurrentSupervisor = Stream.unwrap(
    EnvironmentSupervisor.pipe(
      Effect.map((supervisor) =>
        Stream.merge(
          SubscriptionRef.changes(supervisor.session).pipe(
            Stream.filter((current) => Option.isSome(current) && current.value !== lost),
            Stream.map((current): NextSession => ({
              _tag: "Session",
              session: Option.getOrThrow(current),
              supervisor,
            })),
          ),
          SubscriptionRef.changes(supervisor.state).pipe(
            Stream.filter((state) => state.phase === "blocked" || !state.desired),
            Stream.map((): NextSession => ({ _tag: "Stopped", label: supervisor.target.label })),
          ),
        ),
      ),
    ),
  );
  const removed = SubscriptionRef.changes(registry.entries).pipe(
    Stream.filter((entries) => {
      const entry = entries.get(environmentId);
      if (entry === undefined) return true;
      label = entry.target.label;
      return false;
    }),
    Stream.map((): NextSession => ({ _tag: "Stopped", label })),
  );
  return Stream.merge(registry.followStream(environmentId, onCurrentSupervisor), removed).pipe(
    Stream.runHead,
    Effect.map(Option.getOrElse((): NextSession => ({ _tag: "Stopped", label }))),
  );
}
