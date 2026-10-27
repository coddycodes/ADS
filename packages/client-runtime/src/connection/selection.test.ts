import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Stream from "effect/Stream";

import { EnvironmentSelection, isEnvironmentShown } from "./selection.ts";

describe("EnvironmentSelection", () => {
  it.effect("defaults to unknown selection and leaves its change stream open", () =>
    Effect.gen(function* () {
      const selection = yield* EnvironmentSelection;
      expect(yield* selection.current).toBeNull();
      const changes = yield* Effect.forkChild(Stream.runCollect(selection.changes));
      yield* Effect.yieldNow;
      expect(changes.pollUnsafe()).toBeUndefined();
      yield* Fiber.interrupt(changes);
    }),
  );
  it("only idles known unselected environments", () => {
    const local = EnvironmentId.make("local");
    const remote = EnvironmentId.make("remote");
    expect(isEnvironmentShown(null, local)).toBe(true);
    expect(isEnvironmentShown(local, local)).toBe(true);
    expect(isEnvironmentShown(remote, local)).toBe(false);
  });
});
