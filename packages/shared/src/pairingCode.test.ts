import { describe, expect, it } from "@effect/vitest";
import { REMOTE_PAIRING_CODE_VERSION, type RemotePairingCodePayload } from "@coddy/contracts";

import {
  PairingCodeParseError,
  PairingCodeUnsupportedVersionError,
  buildBrowserPairUrl,
  buildPairingDeepLink,
  encodePairingCode,
  parsePairingCode,
  resolvePairingDeepLinkCode,
} from "./pairingCode.ts";

const payload: RemotePairingCodePayload = {
  v: REMOTE_PAIRING_CODE_VERSION,
  endpoint: "http://192.168.1.20:3773",
  name: "AI-SERVER",
  token: "BCDFGHJKMNPQ",
  hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  reach: "another-device",
  storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
};

describe("pairingCode", () => {
  it("round-trips encode/parse", () => {
    expect(parsePairingCode(encodePairingCode(payload))).toEqual(payload);
  });

  it("parses the deep-link and browser-url forms", () => {
    const code = encodePairingCode(payload);
    expect(parsePairingCode(buildPairingDeepLink(code))).toEqual(payload);
    expect(parsePairingCode(`coddy-studio:/pair?code=${code}`)).toEqual(payload);
    expect(parsePairingCode(buildBrowserPairUrl(payload.endpoint, code))).toEqual(payload);
    expect(buildPairingDeepLink(code)).toBe(`coddy-studio://pair?code=${code}`);
    expect(buildBrowserPairUrl(payload.endpoint, code)).toBe(
      `http://192.168.1.20:3773/pair?code=${code}`,
    );
    expect(() => parsePairingCode(`coddy-studio://other?code=${code}`)).toThrow(PairingCodeParseError);
  });

  it.each([
    ["coddy-studio://pair?code=abc123-_", "abc123-_"],
    ["coddy-studio:/pair?code=abc123-_", "abc123-_"],
    ["https://pair?code=abc123-_", null],
    ["coddy-studio://other?code=abc123-_", null],
    ["coddy-studio:/other?code=abc123-_", null],
    ["coddy-studio://pair", null],
    ["not a url", null],
  ])("resolves pairing deep link %s", (rawUrl, expected) => {
    expect(resolvePairingDeepLinkCode(rawUrl)).toBe(expected);
  });

  it("classifies an unknown version as unsupported", () => {
    const future = Buffer.from(JSON.stringify({ ...payload, v: 99 })).toString("base64url");
    expect(() => parsePairingCode(future)).toThrow(PairingCodeUnsupportedVersionError);
  });

  it("classifies garbage as a parse error", () => {
    expect(() => parsePairingCode("not-base64url-json!!")).toThrow(PairingCodeParseError);
    expect(() => parsePairingCode("coddy-studio://pair?nope=1")).toThrow(PairingCodeParseError);
    const missingField = Buffer.from(JSON.stringify({ v: 1, endpoint: "http://x" })).toString(
      "base64url",
    );
    expect(() => parsePairingCode(missingField)).toThrow(PairingCodeParseError);
  });
});
