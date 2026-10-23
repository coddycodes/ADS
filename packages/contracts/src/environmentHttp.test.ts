import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as HttpServerRespondable from "effect/http/HttpServerRespondable";
import { describe, expect } from "vite-plus/test";

import {
  AuthClientSessionRevokeResult,
  AuthOtherClientSessionsRevokeResult,
  AuthPairingLinkRevokeResult,
  EnvironmentAuthInvalidError,
  EnvironmentCloudEndpointUnavailableError,
  EnvironmentCloudLinkStateResult,
  EnvironmentCloudRelayConfigResult,
  EnvironmentHttpBadRequestError,
  EnvironmentHttpCommonError,
  EnvironmentHttpConflictError,
  EnvironmentHttpForbiddenError,
  EnvironmentHttpInternalServerError,
  EnvironmentHttpUnauthorizedError,
  EnvironmentInternalError,
  EnvironmentOperationForbiddenError,
  EnvironmentRequestInvalidError,
  EnvironmentScopeRequiredError,
} from "./environmentHttp.ts";
import {
  expectDecodeFailure,
  expectEncodeFailure,
  makeInvalidClassInstance,
} from "./test/schemaAssertions.ts";

const commonErrors = [
  new EnvironmentRequestInvalidError({
    code: "invalid_request",
    reason: "invalid_scope",
    traceId: "trace-request",
  }),
  new EnvironmentAuthInvalidError({
    code: "auth_invalid",
    reason: "missing_credential",
    traceId: "trace-auth",
  }),
  new EnvironmentScopeRequiredError({
    code: "insufficient_scope",
    requiredScope: "terminal:operate",
    traceId: "trace-scope",
  }),
  new EnvironmentOperationForbiddenError({
    code: "operation_forbidden",
    reason: "current_session_revoke_not_allowed",
    traceId: "trace-forbidden",
  }),
  new EnvironmentInternalError({
    code: "internal_error",
    reason: "orchestration_dispatch_failed",
    traceId: "trace-internal",
  }),
] as const;

const legacyHttpErrors = [
  new EnvironmentHttpBadRequestError({ message: "bad request" }),
  new EnvironmentHttpUnauthorizedError({ message: "unauthorized" }),
  new EnvironmentHttpForbiddenError({ message: "forbidden" }),
  new EnvironmentHttpInternalServerError({ message: "internal" }),
  new EnvironmentHttpConflictError({ message: "conflict" }),
  new EnvironmentCloudEndpointUnavailableError({
    message: "endpoint unavailable",
    endpointRuntimeStatus: { state: "provisioning" },
  }),
] as const;

const decodeCommonError = Schema.decodeUnknownSync(EnvironmentHttpCommonError);
const encodeCommonError = Schema.encodeUnknownSync(EnvironmentHttpCommonError);
const decodeCloudLinkState = Schema.decodeUnknownSync(EnvironmentCloudLinkStateResult);
const encodeCloudLinkState = Schema.encodeSync(EnvironmentCloudLinkStateResult);
const decodeCloudRelayConfig = Schema.decodeUnknownSync(EnvironmentCloudRelayConfigResult);
const decodePairingLinkRevoke = Schema.decodeUnknownSync(AuthPairingLinkRevokeResult);
const decodeClientSessionRevoke = Schema.decodeUnknownSync(AuthClientSessionRevokeResult);
const decodeOtherClientSessionsRevoke = Schema.decodeUnknownSync(
  AuthOtherClientSessionsRevokeResult,
);

describe("environment HTTP errors", () => {
  it("round-trips every common tagged-error alternative", () => {
    for (const error of commonErrors) {
      const encoded = encodeCommonError(error);
      const decoded = decodeCommonError(encoded);
      expect(decoded._tag).toBe(error._tag);
    }
  });

  it("encodes and decodes legacy HTTP error constructors", () => {
    const schemas = [
      EnvironmentHttpBadRequestError,
      EnvironmentHttpUnauthorizedError,
      EnvironmentHttpForbiddenError,
      EnvironmentHttpInternalServerError,
      EnvironmentHttpConflictError,
      EnvironmentCloudEndpointUnavailableError,
    ] as const;

    for (const [index, error] of legacyHttpErrors.entries()) {
      const schema = schemas[index] as Schema.Top;
      const encoded = Schema.encodeUnknownSync(schema as never)(error);
      const decoded = Schema.decodeUnknownSync(schema as never)(encoded) as {
        readonly _tag: string;
      };
      expect(decoded._tag).toBe(error._tag);
    }
  });

  it.effect("converts every error to its declared HTTP response boundary", () =>
    Effect.gen(function* () {
      const cases = [
        [commonErrors[0], 400],
        [commonErrors[1], 401],
        [commonErrors[2], 403],
        [commonErrors[3], 403],
        [commonErrors[4], 500],
        [legacyHttpErrors[0], 400],
        [legacyHttpErrors[1], 401],
        [legacyHttpErrors[2], 403],
        [legacyHttpErrors[3], 500],
        [legacyHttpErrors[4], 409],
        [legacyHttpErrors[5], 503],
      ] as const;

      for (const [error, expectedStatus] of cases) {
        const response = yield* error[HttpServerRespondable.symbol]();
        assert.strictEqual(response.status, expectedStatus);
        assert.strictEqual(response.body._tag, "Uint8Array");
      }
    }),
  );

  it("reports invalid reasons at the same structured path on decode and encode", () => {
    const invalid = {
      _tag: "EnvironmentRequestInvalidError",
      code: "invalid_request",
      reason: "unknown_reason",
      traceId: "trace-request",
    };
    const expected = {
      rootTag: "AnyOf" as const,
      paths: [["reason"]],
      containsTag: "AnyOf" as const,
    };
    expectDecodeFailure(EnvironmentHttpCommonError, invalid, expected);
    expectEncodeFailure(
      EnvironmentHttpCommonError,
      makeInvalidClassInstance(EnvironmentRequestInvalidError.prototype, invalid),
      expected,
    );
  });
});

describe("environment HTTP result schemas", () => {
  it("round-trips nullable cloud state and operation results", () => {
    const state = {
      linked: false,
      cloudUserId: null,
      relayUrl: null,
      relayIssuer: null,
    } as const;
    expect(encodeCloudLinkState(decodeCloudLinkState(state))).toEqual(state);

    expect(
      decodeCloudRelayConfig({
        ok: true,
        endpointRuntimeStatus: { state: "ready" },
      }).ok,
    ).toBe(true);
    expect(decodePairingLinkRevoke({ revoked: true }).revoked).toBe(true);
    expect(decodeClientSessionRevoke({ revoked: false }).revoked).toBe(false);
    expect(decodeOtherClientSessionsRevoke({ revokedCount: 2 }).revokedCount).toBe(2);
  });

  it("reports invalid result fields by path", () => {
    const invalid = { linked: "yes", cloudUserId: null, relayUrl: null, relayIssuer: null };
    const expected = {
      rootTag: "Composite" as const,
      paths: [["linked"]],
      containsTag: "InvalidType" as const,
    };
    expectDecodeFailure(EnvironmentCloudLinkStateResult, invalid, expected);
    expectEncodeFailure(EnvironmentCloudLinkStateResult, invalid, expected);
  });
});
