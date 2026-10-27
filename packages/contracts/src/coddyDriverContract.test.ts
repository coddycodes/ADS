import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import { PROVIDER_DISPLAY_NAMES } from "./model.ts";
import { ProviderDriverKind } from "./providerInstance.ts";
import { RuntimeEventRaw } from "./providerRuntime.ts";

describe("coddy driver contract", () => {
  it("names the driver and accepts its raw event source", () => {
    const kind = ProviderDriverKind.make("coddy");

    expect(PROVIDER_DISPLAY_NAMES[kind]).toBe("Coddy");
    expect(
      Schema.decodeUnknownSync(RuntimeEventRaw)({ source: "coddy.cli.sse", payload: {} }).source,
    ).toBe("coddy.cli.sse");
  });
});
