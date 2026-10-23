import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Base64Url from "effect/encoding/Base64Url";
import { CompactSign, decodeProtectedHeader, type JWTPayload } from "jose";

import {
  decodeRelayJwt,
  normalizeRelayIssuer,
  RelayJwtError,
  signRelayJwt,
  verifyRelayJwt,
} from "./relayJwt.ts";
import { stableStringify } from "./relaySigning.ts";

const ISSUER = "https://issuer.example.test";
const AUDIENCE = "relay-audience";
const TYP = "test-relay+jwt";
const NOW = 1_700_000_000;
const textEncoder = new TextEncoder();

function toPem(label: "PRIVATE KEY" | "PUBLIC KEY", value: ArrayBuffer): string {
  const base64 = btoa(String.fromCharCode(...new Uint8Array(value)));
  const lines = base64.match(/.{1,64}/gu);
  if (lines === null) throw new Error("WebCrypto exported an empty key.");
  return `-----BEGIN ${label}-----\n${lines.join("\n")}\n-----END ${label}-----`;
}

async function generateRelayKeyPair() {
  const keyPair = (await crypto.subtle.generateKey("Ed25519", true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
  return {
    privateKey: keyPair.privateKey,
    publicKey: keyPair.publicKey,
    privatePem: toPem("PRIVATE KEY", await crypto.subtle.exportKey("pkcs8", keyPair.privateKey)),
    publicPem: toPem("PUBLIC KEY", await crypto.subtle.exportKey("spki", keyPair.publicKey)),
  };
}

const primaryKeyPair = generateRelayKeyPair();
const secondaryKeyPair = generateRelayKeyPair();

function claims(overrides: JWTPayload = {}): JWTPayload {
  return {
    iss: ISSUER,
    aud: AUDIENCE,
    sub: "subject-1",
    jti: "jwt-1",
    iat: NOW,
    nbf: NOW,
    exp: NOW + 120,
    scope: ["relay:read"],
    ...overrides,
  };
}

function withoutClaim(payload: JWTPayload, claim: keyof JWTPayload): JWTPayload {
  const result = { ...payload };
  delete result[claim];
  return result;
}

const getPrimaryKeyPair = Effect.promise(() => primaryKeyPair);
const getSecondaryKeyPair = Effect.promise(() => secondaryKeyPair);

const signClaims = Effect.fn("test.signClaims")(function* (
  payload: JWTPayload,
  privatePem?: string,
) {
  const keyPair = yield* getPrimaryKeyPair;
  return yield* signRelayJwt({
    privateKey: privatePem ?? keyPair.privatePem,
    typ: TYP,
    payload,
  });
});

const verifyToken = Effect.fn("test.verifyToken")(function* (input: {
  readonly token: string;
  readonly publicKey?: string;
  readonly issuer?: string;
  readonly audience?: string;
  readonly nowEpochSeconds?: number;
  readonly maxTokenAge?: string | number;
}) {
  const keyPair = yield* getPrimaryKeyPair;
  return yield* verifyRelayJwt({
    publicKey: input.publicKey ?? keyPair.publicPem,
    token: input.token,
    typ: TYP,
    issuer: input.issuer ?? ISSUER,
    audience: input.audience ?? AUDIENCE,
    nowEpochSeconds: input.nowEpochSeconds ?? NOW,
    ...(input.maxTokenAge === undefined ? {} : { maxTokenAge: input.maxTokenAge }),
  });
});

const verificationError = (input: Parameters<typeof verifyToken>[0]) =>
  verifyToken(input).pipe(Effect.flip);

describe("relayJwt", () => {
  it.effect("signs, decodes, and verifies relay claims with generated Ed25519 keys", () =>
    Effect.gen(function* () {
      const keyPair = yield* getPrimaryKeyPair;
      const escapedPrivatePem = `  ${keyPair.privatePem.replace(/\n/gu, "\\n")}  `;
      const escapedPublicPem = `  ${keyPair.publicPem.replace(/\n/gu, "\\n")}  `;
      const token = yield* signClaims(claims(), escapedPrivatePem);

      expect(decodeProtectedHeader(token)).toEqual({ alg: "EdDSA", typ: TYP });
      expect(decodeRelayJwt(token)).toMatchObject({
        iss: ISSUER,
        aud: AUDIENCE,
        sub: "subject-1",
        jti: "jwt-1",
      });
      expect(yield* verifyToken({ token, publicKey: escapedPublicPem })).toMatchObject({
        sub: "subject-1",
        scope: ["relay:read"],
      });
      expect(yield* verifyToken({ token, maxTokenAge: 300 })).toMatchObject({ jti: "jwt-1" });
    }),
  );

  it("normalizes relay issuers without changing internal URL content", () => {
    expect(normalizeRelayIssuer("  https://issuer.example.test/path///  ")).toBe(
      "https://issuer.example.test/path",
    );
    expect(normalizeRelayIssuer("///")).toBe("");
  });

  it.effect("preserves signing context without exposing private key material", () =>
    Effect.gen(function* () {
      const privateKey = "PRIVATE_KEY_SECRET_VALUE";
      const error = yield* signRelayJwt({
        privateKey,
        typ: "test-sign+jwt",
        payload: { sub: "subject" },
      }).pipe(Effect.flip);

      expect(error).toBeInstanceOf(RelayJwtError);
      expect(error).toMatchObject({ operation: "sign", typ: "test-sign+jwt" });
      expect(error.cause).toBeInstanceOf(Error);
      expect(error.message).toBe('Failed to sign relay JWT of type "test-sign+jwt".');
      expect(error.message).not.toContain(privateKey);
      expect(String(error.cause)).not.toContain(privateKey);
    }),
  );

  it.effect("preserves verification context without exposing tokens or signatures", () =>
    Effect.gen(function* () {
      const token = "secret-header.secret-claims.secret-signature";
      const error = yield* verificationError({ token });

      expect(error).toMatchObject({
        operation: "verify",
        typ: TYP,
        issuer: ISSUER,
        audience: AUDIENCE,
      });
      expect(error.cause).toBeInstanceOf(Error);
      expect(error.message).toBe(`Failed to verify relay JWT of type "${TYP}".`);
      expect(error.message).not.toContain(token);
      expect(String(error.cause)).not.toContain(token);
    }),
  );

  it.effect("enforces EdDSA, signature integrity, and the configured public key", () =>
    Effect.gen(function* () {
      const token = yield* signClaims(claims());
      const [, payload] = token.split(".");
      if (!payload) throw new Error("Expected compact relay JWT fixture.");
      const wrongAlgorithmHeader = Base64Url.encode(
        textEncoder.encode(stableStringify({ alg: "ES256", typ: TYP })),
      );
      const secondary = yield* getSecondaryKeyPair;

      for (const input of [
        { token: `${wrongAlgorithmHeader}.${payload}.signature` },
        { token: token.replace(/[^.]+$/u, Base64Url.encode(new Uint8Array(64))) },
        { token, publicKey: secondary.publicPem },
        { token, publicKey: "not-a-public-key" },
      ]) {
        const error = yield* verificationError(input);
        expect(error).toBeInstanceOf(RelayJwtError);
        expect(RelayJwtError.diagnosticCode(error)).not.toBe("unknown");
      }
    }),
  );

  it.effect("rejects issuer, audience, and required issued-at claim mismatches", () =>
    Effect.gen(function* () {
      const validToken = yield* signClaims(claims());
      expect(
        yield* verificationError({
          token: validToken,
          issuer: "https://other-issuer.example.test",
        }),
      ).toBeInstanceOf(RelayJwtError);
      expect(
        yield* verificationError({ token: validToken, audience: "other-audience" }),
      ).toBeInstanceOf(RelayJwtError);

      const missingIssuer = yield* signClaims(withoutClaim(claims(), "iss"));
      const missingAudience = yield* signClaims(withoutClaim(claims(), "aud"));
      const missingIssuedAt = yield* signClaims(withoutClaim(claims(), "iat"));
      for (const token of [missingIssuer, missingAudience, missingIssuedAt]) {
        expect(yield* verificationError({ token })).toBeInstanceOf(RelayJwtError);
      }
    }),
  );

  it.effect("applies expiry, not-before, and issued-at clock tolerance at exact boundaries", () =>
    Effect.gen(function* () {
      const acceptedClaims = [
        claims({ exp: NOW - 59 }),
        claims({ nbf: NOW + 60 }),
        withoutClaim(claims({ iat: NOW - 360 }), "nbf"),
        withoutClaim(claims({ iat: NOW + 60 }), "nbf"),
      ];
      for (const payload of acceptedClaims) {
        const token = yield* signClaims(payload);
        expect(yield* verifyToken({ token, maxTokenAge: 300 })).toMatchObject({
          iss: ISSUER,
          aud: AUDIENCE,
          sub: "subject-1",
        });
      }

      const rejectedClaims = [
        claims({ exp: NOW - 60 }),
        claims({ nbf: NOW + 61 }),
        withoutClaim(claims({ iat: NOW - 361 }), "nbf"),
        withoutClaim(claims({ iat: NOW + 61 }), "nbf"),
      ];
      for (const payload of rejectedClaims) {
        const token = yield* signClaims(payload);
        expect(yield* verificationError({ token, maxTokenAge: 300 })).toBeInstanceOf(RelayJwtError);
      }
    }),
  );

  it.effect(
    "handles duplicate identical signed claims and duplicate audience entries deterministically",
    () =>
      Effect.gen(function* () {
        const keyPair = yield* getPrimaryKeyPair;
        const duplicateIssuerPayload = `{"iss":"${ISSUER}","iss":"${ISSUER}","aud":["${AUDIENCE}","${AUDIENCE}"],"sub":"subject-1","iat":${NOW},"exp":${NOW + 120}}`;
        const token = yield* Effect.promise(() =>
          new CompactSign(textEncoder.encode(duplicateIssuerPayload))
            .setProtectedHeader({ alg: "EdDSA", typ: TYP })
            .sign(keyPair.privateKey),
        );

        expect(yield* verifyToken({ token })).toMatchObject({
          iss: ISSUER,
          aud: [AUDIENCE, AUDIENCE],
          sub: "subject-1",
        });
      }),
  );

  it.effect("rejects malformed compact encoding, JSON, and signature segments", () =>
    Effect.gen(function* () {
      expect(() => decodeRelayJwt("not-a-jwt")).toThrow();
      const token = yield* signClaims(claims());
      const [header, , signature] = token.split(".");
      if (!header || !signature) throw new Error("Expected compact relay JWT fixture.");
      const malformedJson = Base64Url.encode(textEncoder.encode("{"));

      for (const malformedToken of [
        "one.two",
        `%.${malformedJson}.%`,
        `${header}.${malformedJson}.${signature}`,
      ]) {
        expect(yield* verificationError({ token: malformedToken })).toBeInstanceOf(RelayJwtError);
      }
    }),
  );

  it("extracts stable diagnostic codes across safe fallback shapes", () => {
    const makeError = (cause: unknown) =>
      new RelayJwtError({ operation: "verify", typ: TYP, cause });

    expect(RelayJwtError.diagnosticCode(makeError({ code: "ERR_JWT_EXPIRED" }))).toBe(
      "ERR_JWT_EXPIRED",
    );
    expect(RelayJwtError.diagnosticCode(makeError(new TypeError("safe")))).toBe("TypeError");
    expect(RelayJwtError.diagnosticCode(makeError({}))).toBe("unknown");
    expect(RelayJwtError.diagnosticCode(makeError({ code: 42 }))).toBe("unknown");
    expect(RelayJwtError.diagnosticCode(makeError({ code: "" }))).toBe("unknown");
    expect(RelayJwtError.diagnosticCode(makeError(null))).toBe("unknown");
    expect(RelayJwtError.diagnosticCode(makeError(Object.assign(new Error(), { name: "" })))).toBe(
      "unknown",
    );
  });
});
