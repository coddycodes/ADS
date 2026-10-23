import { EnvironmentSelection } from "@bibcode/client-runtime/connection";
import * as Effect from "effect/Effect";
import { AtomRegistry } from "effect/reactivity";

import { activeEnvironmentIdAtom } from "../state/activeEnvironment";

/** Publishes the environment-rail selection to the connection supervisors. */
export function makeEnvironmentSelection(registry: AtomRegistry.AtomRegistry) {
  return EnvironmentSelection.of({
    current: Effect.sync(() => registry.get(activeEnvironmentIdAtom)),
    changes: AtomRegistry.toStream(registry, activeEnvironmentIdAtom),
  });
}
