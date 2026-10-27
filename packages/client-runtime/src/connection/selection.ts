import type { EnvironmentId } from "@coddy/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";

/**
 * The environment the user is looking at. After long failure, supervisors of
 * other environments move to the idle retry ladder. The default selects
 * nothing, so without a platform value no environment counts as unselected.
 */
export class EnvironmentSelection extends Context.Reference<{
  /** The selected environment now, or null when unknown. */
  readonly current: Effect.Effect<EnvironmentId | null>;
  /** The selected environment after every change; it may repeat the current one. */
  readonly changes: Stream.Stream<EnvironmentId | null>;
}>("@coddy/client-runtime/connection/selection/EnvironmentSelection", {
  defaultValue: () => ({ current: Effect.succeed(null), changes: Stream.never }),
}) {}

/** True when the environment is selected, or when nothing is selected. */
export function isEnvironmentShown(
  selected: EnvironmentId | null,
  environmentId: EnvironmentId,
): boolean {
  return selected === null || selected === environmentId;
}
