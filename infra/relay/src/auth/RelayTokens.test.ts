// @effect-diagnostics-next-line nodeBuiltinImport:off - Test fixtures generate key pairs synchronously.
import * as NodeCrypto from "node:crypto";

import { describe, expect, it } from "@effect/vitest";
import { signRelayJwt } from "@bibcode/shared/relayJwt";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";

import * as RelayConfiguration from "../Config.ts";
import * as RelayTokens from "./RelayTokens.ts";

const keyPair = NodeCrypto.generateKeyPairSync("ed25519", {
  privateKeyEncoding: { format: "pem", type: "pkcs8" },
  publicKeyEncoding: { format: "pem", type: "spki" },
});

const config = RelayConfiguration.RelayConfiguration.of({
  relayIssuer: "https://relay.example.test/",
  clerkSecretKey: Redacted.make("clerk-secret"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "bibcode-relay",
  cloudMintPrivateKey: Redacted.make(keyPair.privateKey),
  cloudMintPublicKey: keyPair.publicKey,
  managedEndpointBaseDomain: undefined,
  managedEndpointNamespace: undefined,
});

const layer = RelayTokens.layer.pipe(Layer.provide(RelayConfiguration.layer(config)));

const invalidPrivateKeyConfig = RelayConfiguration.RelayConfiguration.of({
  ...config,
  cloudMintPrivateKey: Redacted.make("not-a-private-key"),
});

const invalidPrivateKeyLayer = RelayTokens.layer.pipe(
  Layer.provide(RelayConfiguration.layer(invalidPrivateKeyConfig)),
);

describe("RelayTokens", () => {
  it.effect("issues a user-bound environment link challenge", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const token = yield* relayTokens.issueLinkChallenge({
        userId: "user_123",
        request: {
          managedTunnelsEnabled: true,
        },
        jti: "challenge-1",
        issuedAtEpochSeconds: 100,
        expiresAtEpochSeconds: 200,
      });

      expect(
        yield* relayTokens.verifyLinkChallenge({
          token,
          userId: "user_123",
          request: {
            managedTunnelsEnabled: true,
          },
          nowEpochSeconds: 150,
        }),
      ).toMatchObject({ sub: "user_123", jti: "challenge-1" });
      expect(
        yield* relayTokens.verifyLinkChallenge({
          token,
          userId: "attacker",
          request: {
            managedTunnelsEnabled: true,
          },
          nowEpochSeconds: 150,
        }),
      ).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("rejects a link challenge when the managed-tunnel request changes", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const token = yield* relayTokens.issueLinkChallenge({
        userId: "user_123",
        request: {
          managedTunnelsEnabled: true,
        },
        jti: "challenge-managed-tunnels",
        issuedAtEpochSeconds: 100,
        expiresAtEpochSeconds: 200,
      });

      expect(
        yield* relayTokens.verifyLinkChallenge({
          token,
          userId: "user_123",
          request: {
            managedTunnelsEnabled: false,
          },
          nowEpochSeconds: 150,
        }),
      ).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("issues and verifies DPoP access tokens bound to one proof-key thumbprint", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const token = yield* relayTokens.issueDpopAccessToken({
        userId: "user_123",
        proofKeyThumbprint: "proof-key-thumbprint",
        jti: "access-token-1",
        issuedAtEpochSeconds: 100,
        expiresAtEpochSeconds: 10_000,
        clientId: "bibcode-web",
        scopes: ["environment:connect", "environment:status"],
      });

      expect(
        yield* relayTokens.verifyDpopAccessToken({ token, nowEpochSeconds: 700 }),
      ).toMatchObject({
        sub: "user_123",
        cnf: { jkt: "proof-key-thumbprint" },
        client_id: "bibcode-web",
        scope: ["environment:connect", "environment:status"],
      });
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token, nowEpochSeconds: 1_960 }),
      ).toMatchObject({ jti: "access-token-1" });
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token, nowEpochSeconds: 1_961 }),
      ).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("issues tunnel-only DPoP access tokens to web public clients", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const token = yield* relayTokens.issueDpopAccessToken({
        userId: "user_123",
        proofKeyThumbprint: "web-proof-key-thumbprint",
        jti: "web-access-token-1",
        issuedAtEpochSeconds: 100,
        expiresAtEpochSeconds: 200,
        clientId: "bibcode-web",
        scopes: ["environment:connect", "environment:status"],
      });

      expect(
        yield* relayTokens.verifyDpopAccessToken({ token, nowEpochSeconds: 150 }),
      ).toMatchObject({
        client_id: "bibcode-web",
        scope: ["environment:connect", "environment:status"],
        cnf: { jkt: "web-proof-key-thumbprint" },
      });
    }).pipe(Effect.provide(layer)),
  );

  it.effect("treats requested scope as an order-independent set", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      expect(
        relayTokens.resolveDpopAccessTokenScopes({
          clientId: "bibcode-web",
          scope: "environment:status environment:connect environment:status",
        }),
      ).toEqual(["environment:status", "environment:connect"]);
    }).pipe(Effect.provide(layer)),
  );

  it.effect("rejects signed DPoP tokens whose scope is outside the relay policy", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const token = yield* signRelayJwt({
        privateKey: keyPair.privateKey,
        typ: "bibcode-relay-dpop-access+jwt",
        payload: {
          iss: "https://relay.example.test",
          aud: "https://relay.example.test",
          sub: "user_123",
          jti: "access-token-invalid-scope",
          iat: 100,
          exp: 200,
          client_id: "bibcode-web",
          scope: "environment:admin",
          cnf: { jkt: "proof-key-thumbprint" },
        },
      });

      expect(yield* relayTokens.verifyDpopAccessToken({ token, nowEpochSeconds: 150 })).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("fails closed for malformed, forged, and malformed-claim tokens", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const forgedKeyPair = NodeCrypto.generateKeyPairSync("ed25519", {
        privateKeyEncoding: { format: "pem", type: "pkcs8" },
        publicKeyEncoding: { format: "pem", type: "spki" },
      });
      const forgedToken = yield* signRelayJwt({
        privateKey: forgedKeyPair.privateKey,
        typ: "bibcode-relay-dpop-access+jwt",
        payload: {
          iss: "https://relay.example.test",
          aud: "https://relay.example.test",
          sub: "user_123",
          jti: "forged-access-token",
          iat: 100,
          exp: 10_000,
          client_id: "bibcode-web",
          scope: "environment:connect",
          cnf: { jkt: "proof-key-thumbprint" },
        },
      });
      const malformedClaimsToken = yield* signRelayJwt({
        privateKey: keyPair.privateKey,
        typ: "bibcode-relay-dpop-access+jwt",
        payload: {
          iss: "https://relay.example.test",
          aud: "https://relay.example.test",
          sub: "user_123",
          jti: "malformed-access-token",
          iat: 100,
          exp: 10_000,
          client_id: "bibcode-web",
          scope: "environment:connect",
          cnf: {},
        },
      });

      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: "not-a-jwt", nowEpochSeconds: 150 }),
      ).toBeNull();
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: forgedToken, nowEpochSeconds: 150 }),
      ).toBeNull();
      expect(
        yield* relayTokens.verifyDpopAccessToken({
          token: malformedClaimsToken,
          nowEpochSeconds: 150,
        }),
      ).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("rejects tokens with a mismatched issuer, audience, type, or max token age", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const makeToken = (payload: Record<string, unknown>, typ = "bibcode-relay-dpop-access+jwt") =>
        signRelayJwt({
          privateKey: keyPair.privateKey,
          typ,
          payload: {
            iss: "https://relay.example.test",
            aud: "https://relay.example.test",
            sub: "user_123",
            jti: "access-token-rejection",
            iat: 100,
            exp: 10_000,
            client_id: "bibcode-web",
            scope: "environment:connect",
            cnf: { jkt: "proof-key-thumbprint" },
            ...payload,
          },
        });

      const [wrongIssuer, wrongAudience, wrongType, staleToken] = yield* Effect.all([
        makeToken({ iss: "https://attacker.example.test" }),
        makeToken({ aud: "https://attacker.example.test" }),
        makeToken({}, "bibcode-link-challenge+jwt"),
        makeToken({}),
      ]);

      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: wrongIssuer, nowEpochSeconds: 150 }),
      ).toBeNull();
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: wrongAudience, nowEpochSeconds: 150 }),
      ).toBeNull();
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: wrongType, nowEpochSeconds: 150 }),
      ).toBeNull();
      expect(
        yield* relayTokens.verifyDpopAccessToken({ token: staleToken, nowEpochSeconds: 1_961 }),
      ).toBeNull();
    }).pipe(Effect.provide(layer)),
  );

  it.effect("rejects a malformed link challenge and signing failures", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const malformedChallenge = yield* signRelayJwt({
        privateKey: keyPair.privateKey,
        typ: "bibcode-link-challenge+jwt",
        payload: {
          iss: "https://relay.example.test",
          aud: "https://relay.example.test",
          sub: "user_123",
          jti: "malformed-challenge",
          iat: 100,
          exp: 200,
          managedTunnelsEnabled: "true",
        },
      });

      expect(
        yield* relayTokens.verifyLinkChallenge({
          token: malformedChallenge,
          userId: "user_123",
          request: { managedTunnelsEnabled: true },
          nowEpochSeconds: 150,
        }),
      ).toBeNull();
      expect(
        yield* Effect.exit(
          relayTokens.issueLinkChallenge({
            userId: "user_123",
            request: { managedTunnelsEnabled: true },
            jti: "valid-challenge",
            issuedAtEpochSeconds: 100,
            expiresAtEpochSeconds: 200,
          }),
        ),
      ).toMatchObject({ _tag: "Success" });
    }).pipe(Effect.provide(layer)),
  );

  it.effect("surfaces a signing error without exposing the configured private key", () =>
    Effect.gen(function* () {
      const relayTokens = yield* RelayTokens.RelayTokens;
      const message = yield* relayTokens
        .issueDpopAccessToken({
          userId: "user_123",
          proofKeyThumbprint: "proof-key-thumbprint",
          jti: "invalid-key",
          issuedAtEpochSeconds: 100,
          expiresAtEpochSeconds: 200,
          clientId: "bibcode-web",
          scopes: ["environment:connect"],
        })
        .pipe(
          Effect.match({
            onFailure: (error) => error.message,
            onSuccess: () => "unexpected signing success",
          }),
        );

      expect(message).toBe('Failed to sign relay JWT of type "bibcode-relay-dpop-access+jwt".');
      expect(message).not.toContain("not-a-private-key");
    }).pipe(Effect.provide(invalidPrivateKeyLayer)),
  );
});
