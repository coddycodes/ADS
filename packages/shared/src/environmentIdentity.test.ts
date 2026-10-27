import { describe, expect, it } from "vite-plus/test";

import { readCoddyStudioEnvironmentVariable } from "./environmentIdentity.ts";

describe("Coddy Studio environment identity", () => {
  it("reads Coddy Studio values", () => {
    expect(readCoddyStudioEnvironmentVariable({ CODDY_STUDIO_PORT: "1" }, "PORT")).toBe("1");
    expect(readCoddyStudioEnvironmentVariable({}, "PORT")).toBeUndefined();
  });
});
