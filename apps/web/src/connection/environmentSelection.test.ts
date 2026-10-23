import { EnvironmentId } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Stream from "effect/Stream";
import { AtomRegistry } from "effect/reactivity";

import { activeEnvironmentIdAtom } from "../state/activeEnvironment";
import { makeEnvironmentSelection } from "./environmentSelection";

describe("makeEnvironmentSelection", () => {
  it.effect("reads the rail selection and streams its changes", () =>
    Effect.gen(function* () {
      const registry = AtomRegistry.make();
      const selection = makeEnvironmentSelection(registry);
      expect(yield* selection.current).toBeNull();
      const seen: Array<EnvironmentId | null> = [];
      const changes = yield* Effect.forkChild(
        selection.changes.pipe(
          Stream.take(2),
          Stream.runForEach((value) => Effect.sync(() => seen.push(value))),
        ),
      );
      // Write only once the subscription delivered the current value.
      for (let attempt = 0; attempt < 100 && seen.length === 0; attempt += 1) {
        yield* Effect.yieldNow;
      }
      expect(seen).toEqual([null]);
      registry.set(activeEnvironmentIdAtom, EnvironmentId.make("remote-1"));
      yield* Fiber.join(changes);
      expect(seen).toEqual([null, "remote-1"]);
      expect(yield* selection.current).toBe("remote-1");
    }),
  );
});
