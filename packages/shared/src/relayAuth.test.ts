import { describe, expect, it } from "vite-plus/test";
import * as Formatter from "effect/Formatter";
import * as Schema from "effect/Schema";

import {
  ClerkPublishableKeyDecodeError,
  ClerkPublishableKeyFrontendApiError,
  clerkFrontendApiHostnameFromPublishableKey,
  clerkFrontendApiUrlFromPublishableKey,
  isAllowedClerkFrontendApiHostname,
  relayClerkTokenOptions,
} from "./relayAuth.ts";

const clerkPublishableKey = (hostname: string, prefix = "pk_test"): string =>
  `${prefix}_${btoa(`${hostname}$`)}`;
const encodeFrontendApiError = Schema.encodeUnknownSync(ClerkPublishableKeyFrontendApiError);

const captureError = (run: () => unknown): unknown => {
  try {
    run();
  } catch (cause) {
    return cause;
  }
  throw new Error("Expected operation to throw");
};

const expectSafeFrontendApiRejection = (frontendApi: string, expectedHostname?: string): void => {
  const error = captureError(() =>
    clerkFrontendApiHostnameFromPublishableKey(clerkPublishableKey(frontendApi)),
  );
  const encoded = encodeFrontendApiError(error as ClerkPublishableKeyFrontendApiError);

  expect(error).toBeInstanceOf(ClerkPublishableKeyFrontendApiError);
  expect(error).toMatchObject({ keyPrefix: "pk_test", reason: "invalid-url" });
  expect(error).not.toHaveProperty("frontendApi");
  expect(error).not.toHaveProperty("cause");
  if (expectedHostname === undefined) {
    expect(error).not.toHaveProperty("hostname");
  } else {
    expect(error).toMatchObject({ hostname: expectedHostname });
  }
  expect(encoded).not.toHaveProperty("frontendApi");
  expect(encoded).not.toHaveProperty("cause");

  for (const surface of [
    (error as Error).message,
    String(error),
    Formatter.format(error),
    Formatter.format(encoded),
    JSON.stringify(error),
    JSON.stringify(encoded),
  ]) {
    expect(surface).not.toContain(frontendApi);
    for (const character of frontendApi) {
      if (!/[\u0021-\u007e]/u.test(character)) {
        expect(surface).not.toContain(character);
      }
    }
  }
};

describe("Clerk relay auth", () => {
  it("derives a custom Frontend API hostname from a Clerk publishable key", () => {
    expect(
      clerkFrontendApiHostnameFromPublishableKey(clerkPublishableKey("clerk.coddy.id")),
    ).toBe("clerk.coddy.id");
    expect(clerkFrontendApiUrlFromPublishableKey(clerkPublishableKey("clerk.coddy.id"))).toBe(
      "https://clerk.coddy.id",
    );
    expect(
      clerkFrontendApiHostnameFromPublishableKey(
        clerkPublishableKey("live.clerk.accounts.com", "pk_live"),
      ),
    ).toBe("live.clerk.accounts.com");
    expect(
      clerkFrontendApiHostnameFromPublishableKey(
        clerkPublishableKey("custom.clerk.accounts.dev", "pk_other"),
      ),
    ).toBe("custom.clerk.accounts.dev");
  });

  it("preserves Clerk publishable key decoding failures", () => {
    const error = captureError(() => clerkFrontendApiUrlFromPublishableKey("pk_test_%"));

    expect(error).toBeInstanceOf(ClerkPublishableKeyDecodeError);
    expect(error).toMatchObject({ keyPrefix: "pk_test" });
    expect((error as ClerkPublishableKeyDecodeError).cause).toBeInstanceOf(Error);
    expect((error as Error).message).toBe("Failed to decode Clerk publishable key (pk_test).");
  });

  it("reports semantic frontend API failures without inventing a cause", () => {
    const emptyError = captureError(() => clerkFrontendApiUrlFromPublishableKey("pk_test_"));
    const pathFrontendApi = "clerk.coddy.id/path";
    const pathError = captureError(() =>
      clerkFrontendApiUrlFromPublishableKey(clerkPublishableKey(pathFrontendApi)),
    );

    expect(emptyError).toBeInstanceOf(ClerkPublishableKeyFrontendApiError);
    expect(emptyError).toMatchObject({
      keyPrefix: "pk_test",
      reason: "empty",
    });
    expect(emptyError).not.toHaveProperty("frontendApi");
    expect(emptyError).not.toHaveProperty("hostname");
    expect((emptyError as Error & { cause?: unknown }).cause).toBeUndefined();
    expect(pathError).toBeInstanceOf(ClerkPublishableKeyFrontendApiError);
    expect(pathError).toMatchObject({
      keyPrefix: "pk_test",
      hostname: "clerk.coddy.id",
      reason: "contains-path",
    });
    expect(pathError).not.toHaveProperty("frontendApi");
    expect((pathError as Error & { cause?: unknown }).cause).toBeUndefined();
  });

  it("reports URL parser failures without retaining their raw input or cause", () => {
    const frontendApi = "[invalid-host-secret";
    const error = captureError(() =>
      clerkFrontendApiHostnameFromPublishableKey(clerkPublishableKey(frontendApi)),
    );

    expect(error).toBeInstanceOf(ClerkPublishableKeyFrontendApiError);
    expect(error).toMatchObject({
      keyPrefix: "pk_test",
      reason: "invalid-url",
    });
    expect(error).not.toHaveProperty("frontendApi");
    expect(error).not.toHaveProperty("hostname");
    expect(error).not.toHaveProperty("cause");
    expect((error as Error).message).toBe(
      "Invalid Clerk frontend API decoded from publishable key (pk_test; invalid-url).",
    );
    expect(Formatter.format(error)).not.toContain(frontendApi);
    expect(
      Formatter.format(encodeFrontendApiError(error as ClerkPublishableKeyFrontendApiError)),
    ).not.toContain(frontendApi);
  });

  it("rejects hidden characters that URL parsing would remove from allowed-looking hosts", () => {
    const normalizedCases = [
      ["clerk.\u00adcoddy.id", "clerk.coddy.id"],
      ["clerk.\tcoddy.id", "clerk.coddy.id"],
      ["clerk.\rcoddy.id", "clerk.coddy.id"],
      ["clerk.\ncoddy.id", "clerk.coddy.id"],
    ] as const;
    for (const [frontendApi, normalizedHostname] of normalizedCases) {
      expectSafeFrontendApiRejection(frontendApi, normalizedHostname);
    }

    expectSafeFrontendApiRejection("clerk.\0coddy.id");
  });

  it("rejects Unicode, IDNA, and ASCII host representations that are not canonical", () => {
    const normalizedCases = [
      ["\u00e9xample.clerk.accounts.dev", "xn--xample-9ua.clerk.accounts.dev"],
      ["fa\u00df.clerk.accounts.com", "xn--fa-hia.clerk.accounts.com"],
      ["CLERK.CODDY_STUDIO.CODES", "clerk.coddy.id"],
      ["%63lerk.coddy.id", "clerk.coddy.id"],
      ["0x7f.0.0.1", "127.0.0.1"],
    ] as const;
    for (const [frontendApi, normalizedHostname] of normalizedCases) {
      expectSafeFrontendApiRejection(frontendApi, normalizedHostname);
    }

    expectSafeFrontendApiRejection("clerk.\u00a0coddy.id");
    expectSafeFrontendApiRejection("clerk.xn--xample-9ua.accounts.dev");
    expectSafeFrontendApiRejection("xn--a.clerk.accounts.dev");
    expectSafeFrontendApiRejection("clerk.coddy.id.");
  });

  it("retains only a safe hostname from rejected secret-bearing URL components", () => {
    const cases = [
      ["clerk.coddy.id/path-secret", "path-secret"],
      ["user-secret@clerk.coddy.id", "user-secret"],
      ["clerk.coddy.id?token=query-secret", "query-secret"],
      ["clerk.coddy.id#fragment-secret", "fragment-secret"],
      ["clerk.coddy.id:8443", "8443"],
    ] as const;
    for (const [frontendApi, secret] of cases) {
      const error = captureError(() =>
        clerkFrontendApiHostnameFromPublishableKey(clerkPublishableKey(frontendApi)),
      );

      expect(error).toBeInstanceOf(ClerkPublishableKeyFrontendApiError);
      expect(error).toMatchObject({
        hostname: "clerk.coddy.id",
        reason: "contains-path",
      });
      expect(error).not.toHaveProperty("frontendApi");
      expect(error).not.toHaveProperty("cause");
      const formatted = Formatter.format(error);
      const encoded = Formatter.format(
        encodeFrontendApiError(error as ClerkPublishableKeyFrontendApiError),
      );
      for (const surface of [(error as Error).message, String(error), formatted, encoded]) {
        expect(surface).not.toContain(frontendApi);
        expect(surface).not.toContain(secret);
      }
    }
  });

  it("uses fixed context when secret-bearing URL components have no safe hostname", () => {
    const frontendApi = "[userinfo-secret/path-secret";
    const error = captureError(() =>
      clerkFrontendApiHostnameFromPublishableKey(clerkPublishableKey(frontendApi)),
    );

    expect(error).toMatchObject({ keyPrefix: "pk_test", reason: "contains-path" });
    expect(error).not.toHaveProperty("frontendApi");
    expect(error).not.toHaveProperty("hostname");
    expect(error).not.toHaveProperty("cause");
    for (const surface of [
      (error as Error).message,
      String(error),
      Formatter.format(error),
      Formatter.format(encodeFrontendApiError(error as ClerkPublishableKeyFrontendApiError)),
    ]) {
      expect(surface).not.toContain(frontendApi);
      expect(surface).not.toContain("userinfo-secret");
      expect(surface).not.toContain("path-secret");
    }
  });

  it("allows standard Clerk hosts and an exact configured custom hostname", () => {
    expect(isAllowedClerkFrontendApiHostname("example.clerk.accounts.dev", null)).toBe(true);
    expect(isAllowedClerkFrontendApiHostname("example.clerk.accounts.com", null)).toBe(true);
    expect(isAllowedClerkFrontendApiHostname("clerk.coddy.id", "clerk.coddy.id")).toBe(
      true,
    );
    expect(isAllowedClerkFrontendApiHostname("attacker.example", "clerk.coddy.id")).toBe(
      false,
    );
    expect(
      isAllowedClerkFrontendApiHostname("nested.clerk.coddy.id", "clerk.coddy.id"),
    ).toBe(false);
  });

  it("builds uncached Clerk token options without retaining a token", () => {
    expect(relayClerkTokenOptions("relay-template")).toEqual({
      template: "relay-template",
      skipCache: true,
    });
  });

  it("keeps decode error messages free of the rejected key material", () => {
    const rejectedKey = "pk_live_%PRIVATE_KEY_MATERIAL";
    const error = captureError(() => clerkFrontendApiUrlFromPublishableKey(rejectedKey));

    expect(error).toBeInstanceOf(ClerkPublishableKeyDecodeError);
    expect((error as Error).message).not.toContain(rejectedKey);
    expect((error as ClerkPublishableKeyDecodeError).keyPrefix).toBe("pk_live");
  });
});
