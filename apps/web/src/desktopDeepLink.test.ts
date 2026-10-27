import { describe, expect, it } from "vite-plus/test";
import { resolvePairingDeepLinkCode } from "@coddy/shared/pairingCode";

import { resolvePairingDeepLink } from "./desktopDeepLink";

describe("resolvePairingDeepLink", () => {
  it("extracts the code from coddy-studio://pair deep links", () => {
    expect(resolvePairingDeepLink("coddy-studio://pair?code=abc123-_")).toEqual({
      code: "abc123-_",
    });
    expect(resolvePairingDeepLinkCode("coddy-studio:/pair?code=abc123-_")).toBe("abc123-_");
  });

  it("rejects other schemes, hosts, and codeless links", () => {
    expect(resolvePairingDeepLink("https://pair?code=abc")).toBeNull();
    expect(resolvePairingDeepLink("coddy-studio://other?code=abc")).toBeNull();
    expect(resolvePairingDeepLink("coddy-studio://pair")).toBeNull();
    expect(resolvePairingDeepLink("not a url")).toBeNull();
  });
});
