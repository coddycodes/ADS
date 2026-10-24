import { assert, describe, it } from "@effect/vitest";
import { p256 } from "@noble/curves/nist.js";
import * as Base64Url from "effect/encoding/Base64Url";

import {
  computeDpopAccessTokenHash,
  computeDpopJwkThumbprint,
  normalizeDpopHtu,
  type DpopPublicJwk,
  verifyDpopProof,
} from "./dpop.ts";

const textEncoder = new TextEncoder();
const FIXED_P256_PUBLIC_KEY_HEX =
  "046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
const FIXED_PUBLIC_JWK: DpopPublicJwk = {
  kty: "EC",
  crv: "P-256",
  x: "axfR8uEsQkf4vOblY6RA8ncDfYEt6zOg9KE5RdiYwpY",
  y: "T-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU",
};
const FIXED_DPOP_PROOF =
  "eyJ0eXAiOiJkcG9wK2p3dCIsImFsZyI6IkVTMjU2IiwiandrIjp7Imt0eSI6IkVDIiwiY3J2IjoiUC0yNTYiLCJ4IjoiYXhmUjh1RXNRa2Y0dk9ibFk2UkE4bmNEZllFdDZ6T2c5S0U1UmRpWXdwWSIsInkiOiJULU5DNHY0YWY1dU81LXRLZkEtZUZpdk9NMWRyTVY3T3k3WkFhRGVfVWZVIn19.eyJodG0iOiJQT1NUIiwiaHR1IjoiaHR0cHM6Ly9leGFtcGxlLmNvbS9vYXV0aC90b2tlbiIsImp0aSI6ImZpeGVkLXByb29mIiwiaWF0IjoxMDAsImF0aCI6IlB4YS0xd2lmUmxQbDd5R18wb0pOZnpxcTdNZWxtT2ZvbkZnT0ZnYXB6RkkifQ.UAXbIdhAIJ6w0LSMhULkW9Q86HY5bnsucvAGvgETDV4RJkld_hncSz3GvokWL49z5mFDoBnIuhAVnQHbresVPg";

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function encodeJson(value: unknown): string {
  return Base64Url.encode(textEncoder.encode(JSON.stringify(value)));
}

async function generateDpopKeyPair(): Promise<{
  readonly privateKey: CryptoKey;
  readonly publicJwk: DpopPublicJwk;
}> {
  const keyPair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
  const exported = await crypto.subtle.exportKey("jwk", keyPair.publicKey);
  if (exported.kty !== "EC" || exported.crv !== "P-256" || !exported.x || !exported.y) {
    throw new Error("WebCrypto did not export a public P-256 JWK.");
  }
  return {
    privateKey: keyPair.privateKey,
    publicJwk: { kty: "EC", crv: "P-256", x: exported.x, y: exported.y },
  };
}

const primaryKeyPair = generateDpopKeyPair();
const secondaryKeyPair = generateDpopKeyPair();

async function signDpopProof(input: {
  readonly privateKey: CryptoKey;
  readonly publicJwk: unknown;
  readonly method?: string;
  readonly url?: string;
  readonly jti?: string;
  readonly iat?: number;
  readonly accessToken?: string;
  readonly nonce?: string;
  readonly header?: Readonly<Record<string, unknown>>;
  readonly payload?: Readonly<Record<string, unknown>>;
}): Promise<string> {
  const header = encodeJson(
    input.header ?? { typ: "dpop+jwt", alg: "ES256", jwk: input.publicJwk },
  );
  const payload = encodeJson(
    input.payload ?? {
      htm: input.method ?? "POST",
      htu: input.url ?? "https://example.com/oauth/token",
      jti: input.jti ?? "proof-1",
      iat: input.iat ?? 100,
      ...(input.accessToken !== undefined
        ? { ath: computeDpopAccessTokenHash(input.accessToken) }
        : {}),
      ...(input.nonce !== undefined ? { nonce: input.nonce } : {}),
    },
  );
  const signature = await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    input.privateKey,
    textEncoder.encode(`${header}.${payload}`),
  );
  return `${header}.${payload}.${Base64Url.encode(new Uint8Array(signature))}`;
}

async function validFixture(input?: {
  readonly method?: string;
  readonly url?: string;
  readonly jti?: string;
  readonly iat?: number;
  readonly accessToken?: string;
  readonly nonce?: string;
}) {
  const keyPair = await primaryKeyPair;
  return {
    ...keyPair,
    proof: await signDpopProof({ ...input, ...keyPair }),
  };
}

function expectRejected(
  result: ReturnType<typeof verifyDpopProof>,
  reason: string,
): asserts result is { readonly ok: false; readonly reason: string } {
  if (result.ok) assert.fail(`Expected rejection: ${reason}`);
  assert.equal(result.reason, reason);
}

describe("verifyDpopProof", () => {
  it("verifies a generated ES256 proof and returns its signed replay identity", async () => {
    const { proof, publicJwk } = await validFixture({ jti: "replay-id", iat: 100 });
    const thumbprint = computeDpopJwkThumbprint(publicJwk);
    const result = verifyDpopProof({
      proof,
      method: "post",
      url: "HTTPS://EXAMPLE.COM:443/oauth/token?code=secret#fragment",
      nowEpochSeconds: 101,
      expectedThumbprint: thumbprint,
    });

    if (!result.ok) assert.fail(result.reason);
    assert.equal(result.thumbprint, thumbprint);
    assert.equal(result.jti, "replay-id");
    assert.equal(result.iat, 100);
  });

  it("rejects missing and malformed compact proofs", () => {
    for (const proof of [null, undefined, "", "   "]) {
      expectRejected(
        verifyDpopProof({
          proof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        "Missing DPoP proof.",
      );
    }
    for (const proof of ["one.two", "one.two.three.four", ".two.three", "one..three", "one.two."]) {
      expectRejected(
        verifyDpopProof({
          proof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        "Invalid DPoP compact JWT.",
      );
    }
  });

  it("rejects malformed JSON and unsupported header algorithms, curves, and key types", async () => {
    const { proof, privateKey, publicJwk } = await validFixture();
    const [, payload, signature] = proof.split(".");
    if (!payload || !signature) assert.fail("Expected compact DPoP fixture.");

    expectRejected(
      verifyDpopProof({
        proof: `${Base64Url.encode(textEncoder.encode("{"))}.${payload}.${signature}`,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 100,
      }),
      "Invalid DPoP JWT header.",
    );

    const invalidHeaders = [
      { typ: "JWT", alg: "ES256", jwk: publicJwk },
      { typ: "dpop+jwt", alg: "ES384", jwk: publicJwk },
      { typ: "dpop+jwt", alg: "ES256", jwk: { ...publicJwk, crv: "P-384" } },
      { typ: "dpop+jwt", alg: "ES256", jwk: { ...publicJwk, kty: "OKP" } },
      { typ: "dpop+jwt", alg: "ES256", jwk: { ...publicJwk, x: "" } },
    ];
    for (const header of invalidHeaders) {
      const invalidProof = await signDpopProof({ privateKey, publicJwk, header });
      expectRejected(
        verifyDpopProof({
          proof: invalidProof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        "Invalid DPoP JWT header.",
      );
    }
  });

  it("rejects malformed payload encodings and missing or invalid required claims", async () => {
    const { proof, privateKey, publicJwk } = await validFixture();
    const [header, , signature] = proof.split(".");
    if (!header || !signature) assert.fail("Expected compact DPoP fixture.");

    expectRejected(
      verifyDpopProof({
        proof: `${header}.${Base64Url.encode(textEncoder.encode("{"))}.${signature}`,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 100,
      }),
      "Invalid DPoP JWT payload.",
    );

    const invalidPayloads = [
      { htu: "https://example.com/oauth/token", jti: "proof-1", iat: 100 },
      { htm: "POST", htu: "", jti: "proof-1", iat: 100 },
      { htm: "POST", htu: "https://example.com/oauth/token", jti: "", iat: 100 },
      { htm: "POST", htu: "https://example.com/oauth/token", jti: "proof-1", iat: 100.5 },
    ];
    for (const payload of invalidPayloads) {
      const invalidProof = await signDpopProof({ privateKey, publicJwk, payload });
      expectRejected(
        verifyDpopProof({
          proof: invalidProof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        "Invalid DPoP JWT payload.",
      );
    }
  });

  it("enforces thumbprint, method, URL, and access-token bindings", async () => {
    const { proof, publicJwk } = await validFixture({ accessToken: "access-token" });
    const thumbprint = computeDpopJwkThumbprint(publicJwk);

    expectRejected(
      verifyDpopProof({
        proof,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 101,
        expectedThumbprint: "",
      }),
      "DPoP key thumbprint mismatch.",
    );
    expectRejected(
      verifyDpopProof({
        proof,
        method: "GET",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 101,
        expectedThumbprint: thumbprint,
      }),
      "DPoP method mismatch.",
    );
    expectRejected(
      verifyDpopProof({
        proof,
        method: "POST",
        url: "not a URL",
        nowEpochSeconds: 101,
      }),
      "DPoP URL mismatch.",
    );
    expectRejected(
      verifyDpopProof({
        proof,
        method: "POST",
        url: "https://example.com/other",
        nowEpochSeconds: 101,
      }),
      "DPoP URL mismatch.",
    );
    expectRejected(
      verifyDpopProof({
        proof,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 101,
        expectedAccessToken: "",
      }),
      "DPoP access token hash mismatch.",
    );
    assert.equal(
      verifyDpopProof({
        proof,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 101,
        expectedAccessToken: "access-token",
      }).ok,
      true,
    );
  });

  it("enforces default and custom age limits at their exact boundaries", async () => {
    const keyPair = await primaryKeyPair;
    const cases = [
      { iat: 1_005, now: 1_000, ok: true },
      { iat: 1_006, now: 1_000, ok: false },
      { iat: 700, now: 1_000, ok: true },
      { iat: 699, now: 1_000, ok: false },
    ];
    for (const testCase of cases) {
      const proof = await signDpopProof({ ...keyPair, iat: testCase.iat });
      assert.equal(
        verifyDpopProof({
          proof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: testCase.now,
        }).ok,
        testCase.ok,
      );
    }

    const customBoundary = await signDpopProof({ ...keyPair, iat: 999 });
    const customExpired = await signDpopProof({ ...keyPair, iat: 998 });
    assert.equal(
      verifyDpopProof({
        proof: customBoundary,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 1_000,
        maxAgeSeconds: 1,
      }).ok,
      true,
    );
    expectRejected(
      verifyDpopProof({
        proof: customExpired,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 1_000,
        maxAgeSeconds: 1,
      }),
      "DPoP proof is outside the allowed time window.",
    );
  });

  it("rejects signature/key mismatches, malformed signatures, and malformed coordinates", async () => {
    const primary = await primaryKeyPair;
    const secondary = await secondaryKeyPair;
    const mismatchedProof = await signDpopProof({
      privateKey: primary.privateKey,
      publicJwk: secondary.publicJwk,
    });
    expectRejected(
      verifyDpopProof({
        proof: mismatchedProof,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 100,
      }),
      "Invalid DPoP signature.",
    );

    const proof = await signDpopProof(primary);
    const [header, payload] = proof.split(".");
    if (!header || !payload) assert.fail("Expected compact DPoP fixture.");
    for (const [signature, reason] of [
      ["%", "Invalid DPoP proof."],
      [Base64Url.encode(new Uint8Array(63)), "Invalid DPoP signature."],
    ] as const) {
      expectRejected(
        verifyDpopProof({
          proof: `${header}.${payload}.${signature}`,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        reason,
      );
    }

    for (const publicJwk of [
      { ...primary.publicJwk, x: Base64Url.encode(new Uint8Array(31)) },
      { ...primary.publicJwk, y: Base64Url.encode(new Uint8Array(31)) },
      { ...primary.publicJwk, x: "%" },
    ]) {
      const malformedKeyProof = await signDpopProof({ ...primary, publicJwk });
      expectRejected(
        verifyDpopProof({
          proof: malformedKeyProof,
          method: "POST",
          url: "https://example.com/oauth/token",
          nowEpochSeconds: 100,
        }),
        "Invalid DPoP proof.",
      );
    }
  });

  it("rejects private JWK material and accepts signed nonce claims without leaking them", async () => {
    const keyPair = await primaryKeyPair;
    const privateJwk = await crypto.subtle.exportKey("jwk", keyPair.privateKey);
    const privateProof = await signDpopProof({ ...keyPair, publicJwk: privateJwk });
    expectRejected(
      verifyDpopProof({
        proof: privateProof,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 100,
      }),
      "Invalid DPoP JWT header.",
    );

    const nonce = "nonce-secret-value";
    const { proof } = await validFixture({ nonce, jti: "same-jti" });
    const first = verifyDpopProof({
      proof,
      method: "POST",
      url: "https://example.com/oauth/token",
      nowEpochSeconds: 100,
    });
    const second = verifyDpopProof({
      proof,
      method: "POST",
      url: "https://example.com/oauth/token",
      nowEpochSeconds: 100,
    });
    assert.equal(first.ok, true);
    assert.equal(second.ok, true);
    if (!first.ok || !second.ok) assert.fail("Expected stateless proof verification.");
    assert.equal(first.jti, second.jti);
    assert.equal(JSON.stringify(first).includes(nonce), false);
  });
});

describe("DPoP helpers", () => {
  it("preserves fixed P-256, JWK, SHA-256, and proof-verification vectors", () => {
    const privateKey = new Uint8Array(32);
    privateKey[31] = 1;

    assert.equal(bytesToHex(p256.getPublicKey(privateKey, false)), FIXED_P256_PUBLIC_KEY_HEX);
    assert.equal(
      computeDpopJwkThumbprint(FIXED_PUBLIC_JWK),
      "xx0BcA-wMohw8atYDJOe6peGModklG2wRHBlXHMvl0M",
    );
    assert.equal(computeDpopAccessTokenHash(""), "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
    assert.equal(
      computeDpopAccessTokenHash("access-token"),
      "Pxa-1wifRlPl7yG_0oJNfzqq7MelmOfonFgOFgapzFI",
    );
    assert.deepEqual(
      verifyDpopProof({
        proof: FIXED_DPOP_PROOF,
        method: "POST",
        url: "https://example.com/oauth/token",
        nowEpochSeconds: 100,
        expectedThumbprint: "xx0BcA-wMohw8atYDJOe6peGModklG2wRHBlXHMvl0M",
        expectedAccessToken: "access-token",
      }),
      {
        ok: true,
        thumbprint: "xx0BcA-wMohw8atYDJOe6peGModklG2wRHBlXHMvl0M",
        jti: "fixed-proof",
        iat: 100,
      },
    );
  });

  it("normalizes URL case/default ports and rejects malformed URLs", () => {
    assert.equal(
      normalizeDpopHtu("HTTPS://EXAMPLE.COM:443/token?authorization=secret#fragment"),
      "https://example.com/token",
    );
    assert.equal(normalizeDpopHtu("not a URL"), null);
  });

  it("computes deterministic, input-sensitive thumbprints and access-token hashes", async () => {
    const primary = await primaryKeyPair;
    const secondary = await secondaryKeyPair;

    assert.equal(
      computeDpopJwkThumbprint(primary.publicJwk),
      computeDpopJwkThumbprint(primary.publicJwk),
    );
    assert.notEqual(
      computeDpopJwkThumbprint(primary.publicJwk),
      computeDpopJwkThumbprint(secondary.publicJwk),
    );
    assert.equal(computeDpopAccessTokenHash(""), computeDpopAccessTokenHash(""));
    assert.notEqual(
      computeDpopAccessTokenHash("access-token"),
      computeDpopAccessTokenHash("other-access-token"),
    );
  });
});
