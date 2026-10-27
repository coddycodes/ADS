import { encodePairingCode } from "@coddy/shared/pairingCode";
import { describe, expect, it } from "vite-plus/test";

import { extractEmbeddedPairingToken } from "./pairingCodeCredential";

describe("extractEmbeddedPairingToken", () => {
  it("returns the one-time token embedded in a valid pairing code", () => {
    const code = encodePairingCode({
      v: 1,
      endpoint: "http://192.168.1.20:3773",
      name: "AI-SERVER",
      token: "BCDFGHJKMNPQ",
      hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
      reach: "another-device",
      storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
    });
    expect(extractEmbeddedPairingToken(code)).toBe("BCDFGHJKMNPQ");
  });

  it("returns null for garbage instead of throwing", () => {
    expect(extractEmbeddedPairingToken("not-a-code!!")).toBeNull();
  });
});
