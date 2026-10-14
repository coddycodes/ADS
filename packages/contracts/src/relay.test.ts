import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";
import * as OpenApi from "effect/unstable/httpapi/OpenApi";

import {
  RelayApi,
  RelayAuthInvalidError,
  RelayEnvironmentConfigRequest,
  RelayEnvironmentConnectNotAuthorizedError,
  RelayEnvironmentEndpointTimedOutError,
  RelayEnvironmentEndpointUnavailableError,
  RelayEnvironmentLinkFailedError,
  RelayEnvironmentLinkProofExpiredError,
  RelayEnvironmentLinkProofInvalidError,
  RelayEnvironmentLinkUnavailableError,
  RelayInternalError,
  RelayManagedEndpointProviderKind,
  RelayProtectedError,
} from "./relay.ts";
import {
  expectDecodeFailure,
  expectEncodeFailure,
  makeInvalidClassInstance,
} from "./test/schemaAssertions.ts";

const decodeProviderKind = Schema.decodeUnknownSync(RelayManagedEndpointProviderKind);
const decodeEnvironmentConfig = Schema.decodeUnknownSync(RelayEnvironmentConfigRequest);
const encodeEnvironmentConfig = Schema.encodeSync(RelayEnvironmentConfigRequest);
const decodeProtectedError = Schema.decodeUnknownSync(RelayProtectedError);
const encodeProtectedError = Schema.encodeUnknownSync(RelayProtectedError);

const protectedErrors = [
  new RelayAuthInvalidError({
    code: "auth_invalid",
    reason: "invalid_dpop",
    traceId: "trace-auth",
  }),
  new RelayEnvironmentLinkProofExpiredError({
    code: "environment_link_proof_expired",
    traceId: "trace-expired",
  }),
  new RelayEnvironmentLinkProofInvalidError({
    code: "environment_link_proof_invalid",
    reason: "replayed_nonce",
    traceId: "trace-invalid",
  }),
  new RelayEnvironmentConnectNotAuthorizedError({
    code: "environment_connect_not_authorized",
    traceId: "trace-not-authorized",
  }),
  new RelayEnvironmentEndpointUnavailableError({
    code: "environment_endpoint_unavailable",
    reason: "endpoint_response_invalid",
    traceId: "trace-unavailable",
  }),
  new RelayEnvironmentEndpointTimedOutError({
    code: "environment_endpoint_timed_out",
    traceId: "trace-timeout",
  }),
  new RelayEnvironmentLinkFailedError({
    code: "environment_link_failed",
    reason: "credential_persistence_failed",
    traceId: "trace-link-failed",
  }),
  new RelayEnvironmentLinkUnavailableError({
    code: "environment_link_unavailable",
    reason: "managed_endpoint_not_configured",
    traceId: "trace-link-unavailable",
  }),
  new RelayInternalError({
    code: "internal_error",
    reason: "database_unavailable",
    traceId: "trace-internal",
  }),
] as const;

describe("RelayApi security", () => {
  it("describes DPoP access tokens using the HTTP DPoP authorization scheme", () => {
    const document = OpenApi.fromApi(RelayApi);

    expect(document.components.securitySchemes?.relayDpop).toEqual({
      type: "http",
      scheme: "DPoP",
      description: "DPoP-bound access token. Requests must also include the DPoP proof JWT header.",
    });
  });
});

describe("relay schemas", () => {
  it("decodes every endpoint provider literal and optional runtime fields", () => {
    expect(
      ["manual", "cloudflare_tunnel", "bibcode_relay"].map((kind) => decodeProviderKind(kind)),
    ).toEqual(["manual", "cloudflare_tunnel", "bibcode_relay"]);

    const minimal = decodeEnvironmentConfig({
      relayUrl: "https://relay.example.test",
      cloudUserId: "user-1",
      environmentCredential: "credential",
      cloudMintPublicKey: "public-key",
      endpointRuntime: null,
    });
    const configured = decodeEnvironmentConfig({
      relayUrl: "https://relay.example.test",
      relayIssuer: "https://issuer.example.test",
      cloudUserId: "user-1",
      environmentCredential: "credential",
      cloudMintPublicKey: "public-key",
      endpointRuntime: {
        providerKind: "cloudflare_tunnel",
        connectorToken: "connector-token",
        tunnelId: "tunnel-1",
        tunnelName: "primary",
      },
    });

    const relink = decodeEnvironmentConfig({
      relayUrl: "https://relay.example.test",
      cloudUserId: "user-1",
      environmentCredential: "credential",
      cloudMintPublicKey: "public-key",
      endpointRuntime: {
        providerKind: "cloudflare_tunnel",
        connectorToken: "connector-token",
        hostname: "env.zone.test",
        previewHostnames: ["env-p1.zone.test", "env-p2.zone.test"],
      },
      requireLinkGeneration: "generation-1",
    });

    expect(minimal.relayIssuer).toBeUndefined();
    expect(minimal.requireLinkGeneration).toBeUndefined();
    expect(configured.endpointRuntime?.tunnelName).toBe("primary");
    expect(configured.endpointRuntime?.previewHostnames).toBeUndefined();
    expect(encodeEnvironmentConfig(configured)).toEqual(configured);
    expect(encodeEnvironmentConfig(relink)).toEqual(relink);
  });

  it("reports invalid provider kinds on decode and encode", () => {
    const expected = { rootTag: "AnyOf" as const };
    expectDecodeFailure(RelayManagedEndpointProviderKind, "automatic", expected);
    expectEncodeFailure(RelayManagedEndpointProviderKind, "automatic", expected);
  });
});

describe("relay protected errors", () => {
  it("constructs every alternative with its public message", () => {
    expect(protectedErrors.map((error) => error.message)).toEqual([
      "Relay authentication failed: invalid_dpop",
      "Relay environment link proof expired",
      "Relay environment link proof is invalid: replayed_nonce",
      "Relay environment connection is not authorized",
      "Relay environment endpoint is unavailable: endpoint_response_invalid",
      "Relay environment endpoint request timed out",
      "Relay environment link failed: credential_persistence_failed",
      "Relay environment link is unavailable: managed_endpoint_not_configured",
      "Relay internal error: database_unavailable",
    ]);
  });

  it("round-trips every tagged union alternative", () => {
    for (const error of protectedErrors) {
      const encoded = encodeProtectedError(error);
      const decoded = decodeProtectedError(encoded);
      expect(decoded._tag).toBe(error._tag);
    }
  });

  it("reports invalid reason paths through the union on decode and encode", () => {
    const invalid = {
      _tag: "RelayAuthInvalidError",
      code: "auth_invalid",
      reason: "expired",
      traceId: "trace-auth",
    };
    const expected = {
      rootTag: "AnyOf" as const,
      paths: [["reason"]],
      containsTag: "AnyOf" as const,
    };
    expectDecodeFailure(RelayProtectedError, invalid, expected);
    expectEncodeFailure(
      RelayProtectedError,
      makeInvalidClassInstance(RelayAuthInvalidError.prototype, invalid),
      expected,
    );
  });
});
