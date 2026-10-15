import { describe, expect, it } from "vite-plus/test";

import { deviceConcurrencyKey } from "./device.ts";

describe("device state commands", () => {
  it("orders commands per device of one environment", () => {
    const key = (environmentId: string, deviceId: string) =>
      deviceConcurrencyKey({ environmentId, input: { deviceId } });
    expect(key("environment-1", "d-1")).toBe(key("environment-1", "d-1"));
    expect(key("environment-1", "d-1")).not.toBe(key("environment-1", "d-2"));
    expect(key("environment-1", "d-1")).not.toBe(key("environment-2", "d-1"));
  });
});
