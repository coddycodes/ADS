import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Headers from "effect/http/Headers";
import { httpHeaderRedactionLayer } from "./httpObservability.ts";

describe("httpHeaderRedactionLayer", () => {
  it.effect("preserves the standard redactions and adds DPoP", () =>
    Effect.gen(function* () {
      const names = yield* Headers.CurrentRedactedNames.pipe(
        Effect.provide(httpHeaderRedactionLayer),
      );

      assert.deepStrictEqual(names, [
        "authorization",
        "cookie",
        "set-cookie",
        "x-api-key",
        "AWSAccessKeyId",
        "Signature",
        "sig",
        "X-Goog-Signature",
        "dpop",
      ]);
    }),
  );

  it.effect("extends a caller-provided redaction set", () => {
    const layer = httpHeaderRedactionLayer.pipe(
      Layer.provide(Layer.succeed(Headers.CurrentRedactedNames, ["x-custom-secret"])),
    );

    return Effect.gen(function* () {
      const names = yield* Headers.CurrentRedactedNames.pipe(Effect.provide(layer));
      assert.deepStrictEqual(names, ["x-custom-secret", "dpop"]);
    });
  });
});
