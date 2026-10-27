import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import { ServerSettings } from "./settings.ts";

const decodeSettings = Schema.decodeUnknownSync(ServerSettings);

describe("coddy provider settings", () => {
  it("fills the coddy instance defaults", () => {
    const parsed = decodeSettings({ providers: { coddy: { binaryPath: "/opt/coddy" } } });

    expect(parsed.providers.coddy.binaryPath).toBe("/opt/coddy");
    expect(parsed.providers.coddy.dataDir).toBe("");
    expect(parsed.providers.coddy.serverUrl).toBe("");
  });
});
