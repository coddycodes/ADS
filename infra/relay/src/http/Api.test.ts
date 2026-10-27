import { createClerkClient, verifyToken } from "@clerk/backend";
import { describe, expect, it } from "@effect/vitest";
import { vi } from "vite-plus/test";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";
import * as HttpRouter from "effect/http/HttpRouter";
import * as HttpServerRequest from "effect/http/HttpServerRequest";

import {
  relayCors,
  relayDocsRedirectRoute,
  relayNotFoundRoute,
  clerkVerificationFailureReason,
  hasExpectedClerkAudience,
  isDpopAuthorizationHeader,
  readHttpAuthorizationCredential,
  resolveConnectClientKeyThumbprint,
  safeAuthFailureReason,
  requireDpopProof,
  requireDpopThumbprint,
  unlinkEnvironment,
  verifyRelayClientBearerToken,
} from "./Api.ts";
import * as DpopProofs from "../auth/DpopProofs.ts";
import * as RelayConfiguration from "../Config.ts";
import * as EnvironmentCredentials from "../environments/EnvironmentCredentials.ts";
import * as EnvironmentLinks from "../environments/EnvironmentLinks.ts";
import * as ManagedEndpointAllocations from "../environments/ManagedEndpointAllocations.ts";
import * as ManagedEndpointProvider from "../environments/ManagedEndpointProvider.ts";

vi.mock("@clerk/backend", () => ({
  createClerkClient: vi.fn(),
  verifyToken: vi.fn(),
}));

const relaySettings: RelayConfiguration.RelayConfiguration["Service"] = {
  relayIssuer: "https://relay.example.test",
  clerkSecretKey: Redacted.make("clerk-secret-key"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "coddy-studio-relay",
  cloudMintPrivateKey: Redacted.make("cloud-mint-private-key"),
  cloudMintPublicKey: "cloud-mint-public-key",
  managedEndpointBaseDomain: undefined,
  managedEndpointNamespace: undefined,
};

describe("relay client authentication", () => {
  it("normalizes authorization headers and safe diagnostic reasons", () => {
    expect(isDpopAuthorizationHeader(undefined)).toBe(false);
    expect(isDpopAuthorizationHeader("Bearer token")).toBe(false);
    expect(isDpopAuthorizationHeader("dpop token")).toBe(true);
    expect(readHttpAuthorizationCredential(Redacted.make("   token"))).toBe("token");

    expect(safeAuthFailureReason("jwt.expired-1")).toBe("jwt.expired-1");
    expect(safeAuthFailureReason("unsafe reason!")).toBe("unknown");
  });

  it("classifies Clerk verification failures without leaking unsafe details", () => {
    expect(clerkVerificationFailureReason(new Error("Invalid JWT audience claim relay"))).toBe(
      "audience_mismatch",
    );
    expect(
      clerkVerificationFailureReason(new Error("Invalid JWT audience claim array relay")),
    ).toBe("audience_mismatch");
    expect(clerkVerificationFailureReason({ reason: "token_expired" })).toBe("token_expired");
    expect(clerkVerificationFailureReason({ reason: "unsafe reason!" })).toBe("unknown");
    expect(clerkVerificationFailureReason({ reason: "" })).toBe("unknown");
    expect(clerkVerificationFailureReason(new TypeError("failed"))).toBe("TypeError");
    expect(clerkVerificationFailureReason(null)).toBe("unknown");
  });

  it("accepts only the configured Clerk audience", () => {
    expect(hasExpectedClerkAudience("relay", "relay")).toBe(true);
    expect(hasExpectedClerkAudience("other", "relay")).toBe(false);
    expect(hasExpectedClerkAudience(["other", "relay"], "relay")).toBe(true);
    expect(hasExpectedClerkAudience([1, "other"], "relay")).toBe(false);
    expect(hasExpectedClerkAudience(null, "relay")).toBe(false);
  });

  it.effect("preserves the existing Clerk session JWT path", () =>
    Effect.gen(function* () {
      vi.mocked(verifyToken).mockResolvedValue({
        sub: "user_session",
        aud: relaySettings.clerkJwtAudience,
      } as never);

      expect(yield* verifyRelayClientBearerToken(relaySettings, "session-token")).toEqual({
        sub: "user_session",
        mode: "clerk_session_bearer",
      });
      expect(verifyToken).toHaveBeenCalledWith("session-token", {
        secretKey: "clerk-secret-key",
        audience: relaySettings.clerkJwtAudience,
      });
      expect(createClerkClient).not.toHaveBeenCalled();
    }).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          vi.mocked(verifyToken).mockReset();
          vi.mocked(createClerkClient).mockReset();
        }),
      ),
    ),
  );

  it.effect("falls back to Clerk OAuth token verification for the headless CLI", () =>
    Effect.gen(function* () {
      vi.mocked(verifyToken).mockRejectedValue(new Error("not a session JWT"));
      vi.mocked(createClerkClient).mockReturnValue({
        authenticateRequest: vi.fn().mockResolvedValue({
          isAuthenticated: true,
          toAuth: () => ({ userId: "user_oauth" }),
        }),
      } as never);

      expect(yield* verifyRelayClientBearerToken(relaySettings, "oauth-token")).toEqual({
        sub: "user_oauth",
        mode: "clerk_oauth_bearer",
      });
      expect(createClerkClient).toHaveBeenCalledWith({
        secretKey: "clerk-secret-key",
        publishableKey: "pk_test_test",
      });
    }).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          vi.mocked(verifyToken).mockReset();
          vi.mocked(createClerkClient).mockReset();
        }),
      ),
    ),
  );

  it.effect("falls back when a Clerk session token lacks the relay audience", () =>
    Effect.gen(function* () {
      vi.mocked(verifyToken).mockResolvedValue({ sub: "user_session", aud: "other" } as never);
      vi.mocked(createClerkClient).mockReturnValue({
        authenticateRequest: vi.fn().mockResolvedValue({
          isAuthenticated: true,
          toAuth: () => ({ userId: "user_oauth" }),
        }),
      } as never);

      expect(yield* verifyRelayClientBearerToken(relaySettings, "oauth-token")).toEqual({
        sub: "user_oauth",
        mode: "clerk_oauth_bearer",
      });
    }).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          vi.mocked(verifyToken).mockReset();
          vi.mocked(createClerkClient).mockReset();
        }),
      ),
    ),
  );

  it.effect("rejects unauthenticated Clerk OAuth token states", () =>
    Effect.gen(function* () {
      vi.mocked(verifyToken).mockRejectedValue(new Error("not a session JWT"));
      const authenticateRequest = vi
        .fn()
        .mockResolvedValueOnce({
          isAuthenticated: false,
          toAuth: () => ({ userId: "unexpected" }),
        })
        .mockResolvedValueOnce({
          isAuthenticated: true,
          toAuth: () => ({ userId: null }),
        });
      vi.mocked(createClerkClient).mockReturnValue({ authenticateRequest } as never);

      const unauthenticated = yield* Effect.flip(
        verifyRelayClientBearerToken(relaySettings, "oauth-token"),
      );
      const missingUser = yield* Effect.flip(
        verifyRelayClientBearerToken(relaySettings, "oauth-token"),
      );

      expect(unauthenticated.message).toBe("Clerk token verification failed");
      expect(missingUser.message).toBe("Clerk token verification failed");
    }).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          vi.mocked(verifyToken).mockReset();
          vi.mocked(createClerkClient).mockReset();
        }),
      ),
    ),
  );
});

describe("relay environment unlink", () => {
  it.effect("retries orphan cleanup after the link was already revoked", () => {
    let linked = true;
    let credentialAttempts = 0;
    let deprovisioned = 0;
    let generation = 0;
    const links = EnvironmentLinks.EnvironmentLinks.of({
      upsert: () => Effect.die("unused"),
      listUsersForEnvironment: () => Effect.die("unused"),
      listPublicKeysForEnvironment: () => Effect.die("unused"),
      listForUser: () => Effect.die("unused"),
      getForUser: () =>
        Effect.succeed(
          linked
            ? {
                environmentId:
                  "env-retry" as EnvironmentLinks.RelayLinkedEnvironmentRecord["environmentId"],
                environmentPublicKey: "public-key",
                label: "Retry",
                endpoint: {
                  httpBaseUrl: "https://env.example.test/",
                  wsBaseUrl: "wss://env.example.test/ws",
                  providerKind: "cloudflare_tunnel",
                },
                managedTunnelsEnabled: true,
                linkedAt: "2026-07-14T00:00:00.000Z",
              }
            : null,
        ),
      restoreForUser: () => Effect.die("unused"),
      revokeForUser: () =>
        Effect.sync(() => {
          const wasLinked = linked;
          linked = false;
          return wasLinked;
        }),
    });
    const credentialFailure =
      new EnvironmentCredentials.EnvironmentCredentialRevokePersistenceError({
        environmentId: "env-retry",
        cause: new Error("database unavailable"),
      });
    const credentials = EnvironmentCredentials.EnvironmentCredentials.of({
      create: () => Effect.die("unused"),
      rotate: () => Effect.die("unused"),
      rollbackRotation: () => Effect.die("unused"),
      authenticate: () => Effect.die("unused"),
      revokeForEnvironmentPublicKey: () => Effect.die("unused"),
      revokeOrphanedForEnvironment: () =>
        Effect.suspend(() =>
          credentialAttempts++ === 0 ? Effect.fail(credentialFailure) : Effect.succeed(true),
        ),
    });
    const provider = ManagedEndpointProvider.ManagedEndpointProvider.of({
      provision: () => Effect.die("unused"),
      deprovision: (input) =>
        Effect.sync(() => {
          expect(input.ownership?.generation).toBe(2);
          deprovisioned++;
        }),
    });
    const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
      withOperation: (input, use) =>
        use({ ...input, generation: ++generation, ownerToken: `owner-${generation}` }),
      acquireOperation: () => Effect.die("unused"),
      releaseOperation: () => Effect.die("unused"),
      renewOperation: () => Effect.die("unused"),
      claimForOperation: () => Effect.die("unused"),
      get: () => Effect.die("unused"),
      reserve: () => Effect.die("unused"),
      recordTunnel: () => Effect.die("unused"),
      recordDns: () => Effect.die("unused"),
      markReady: () => Effect.die("unused"),
      remove: () => Effect.die("unused"),
    });
    const run = unlinkEnvironment({ userId: "user-1", environmentId: "env-retry" }).pipe(
      Effect.provideService(EnvironmentLinks.EnvironmentLinks, links),
      Effect.provideService(EnvironmentCredentials.EnvironmentCredentials, credentials),
      Effect.provideService(ManagedEndpointProvider.ManagedEndpointProvider, provider),
      Effect.provideService(ManagedEndpointAllocations.ManagedEndpointAllocations, allocations),
    );

    return Effect.gen(function* () {
      const first = yield* Effect.flip(run);
      expect(first).toBe(credentialFailure);
      expect(linked).toBe(false);
      expect(deprovisioned).toBe(0);

      expect(yield* run).toEqual({ ok: true });
      expect(credentialAttempts).toBe(2);
      expect(deprovisioned).toBe(1);
    });
  });
});

describe("relay DPoP binding forwarding", () => {
  it("resolves compatible client proof-key payload variants", () => {
    expect(resolveConnectClientKeyThumbprint({ clientKeyThumbprint: "key" })).toBe("key");
    expect(resolveConnectClientKeyThumbprint({ clientProofKeyThumbprint: "legacy" })).toBe(
      "legacy",
    );
    expect(
      resolveConnectClientKeyThumbprint({
        clientKeyThumbprint: "same",
        clientProofKeyThumbprint: "same",
      }),
    ).toBe("same");
    expect(
      resolveConnectClientKeyThumbprint({
        clientKeyThumbprint: "new",
        clientProofKeyThumbprint: "legacy",
      }),
    ).toBeNull();
    expect(resolveConnectClientKeyThumbprint({})).toBeNull();
  });

  it.effect("forwards present empty bindings and omits absent access-token bindings", () =>
    Effect.gen(function* () {
      const inputs: Array<
        Parameters<DpopProofs.DpopProofReplay["Service"]["verifyAndConsume"]>[0]
      > = [];
      const replay = DpopProofs.DpopProofReplay.of({
        verifyAndConsume: (input) =>
          Effect.sync(() => {
            inputs.push(input);
            return "verified-thumbprint";
          }),
        consume: () => Effect.die("unexpected replay consumption"),
        pruneExpired: Effect.die("unexpected replay pruning"),
      });
      const request = HttpServerRequest.fromWeb(
        new Request("https://relay.example.test/v1/environments/env/connect?client=web", {
          method: "POST",
          headers: { dpop: "signed-proof" },
        }),
      );
      const provideRequest = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
        effect.pipe(
          Effect.provideService(DpopProofs.DpopProofReplay, replay),
          Effect.provideService(HttpServerRequest.HttpServerRequest, request),
        );

      yield* provideRequest(requireDpopThumbprint("", { expectedAccessToken: "" }));
      yield* provideRequest(requireDpopProof({ expectedAccessToken: "" }));
      yield* provideRequest(requireDpopThumbprint("expected-thumbprint"));
      yield* provideRequest(requireDpopProof());

      expect(inputs[0]).toMatchObject({
        expectedThumbprint: "",
        expectedAccessToken: "",
      });
      expect(inputs[1]).toMatchObject({ expectedAccessToken: "" });
      expect(inputs[1]).not.toHaveProperty("expectedThumbprint");
      expect(inputs[2]).toMatchObject({ expectedThumbprint: "expected-thumbprint" });
      expect(inputs[2]).not.toHaveProperty("expectedAccessToken");
      expect(inputs[3]).not.toHaveProperty("expectedThumbprint");
      expect(inputs[3]).not.toHaveProperty("expectedAccessToken");
    }),
  );
});

describe("relay routing fallback", () => {
  it.effect("answers CORS preflight requests without running the route", () =>
    Effect.gen(function* () {
      const request = HttpServerRequest.fromWeb(
        new Request("https://relay.test/v1/environments", { method: "OPTIONS" }),
      );
      const httpEffect = yield* HttpRouter.toHttpEffect(Layer.merge(relayNotFoundRoute, relayCors));
      const response = yield* httpEffect.pipe(
        Effect.provideService(HttpServerRequest.HttpServerRequest, request),
      );

      expect(response.status).toBe(204);
      expect(response.headers["access-control-allow-methods"]).toContain("OPTIONS");
      expect(response.headers["access-control-allow-headers"]).toContain("dpop");
    }).pipe(Effect.scoped),
  );

  it.effect("redirects the relay root to the API docs", () =>
    Effect.gen(function* () {
      const request = HttpServerRequest.fromWeb(new Request("https://relay.test/"));
      const httpEffect = yield* HttpRouter.toHttpEffect(
        Layer.mergeAll(relayDocsRedirectRoute, relayNotFoundRoute, relayCors),
      );
      const response = yield* httpEffect.pipe(
        Effect.provideService(HttpServerRequest.HttpServerRequest, request),
      );

      expect(response.status).toBe(302);
      expect(response.headers.location).toBe("/docs");
      expect(response.headers["access-control-allow-origin"]).toBe("*");
    }).pipe(Effect.scoped),
  );

  it.effect("returns a CORS-compatible 404 response for unmatched paths", () =>
    Effect.gen(function* () {
      const request = HttpServerRequest.fromWeb(
        new Request("https://relay.test/v1/environmentsd", { method: "GET" }),
      );
      const httpEffect = yield* HttpRouter.toHttpEffect(Layer.merge(relayNotFoundRoute, relayCors));
      const response = yield* httpEffect.pipe(
        Effect.provideService(HttpServerRequest.HttpServerRequest, request),
      );

      expect(response.status).toBe(404);
      expect(response.headers["access-control-allow-origin"]).toBe("*");
    }).pipe(Effect.scoped),
  );
});
