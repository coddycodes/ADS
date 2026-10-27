// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";

import * as Schema from "effect/Schema";
import type * as SchemaAST from "effect/SchemaAST";
import * as HttpApi from "effect/http-api/HttpApi";
import type * as HttpApiEndpoint from "effect/http-api/HttpApiEndpoint";
import * as HttpApiSchema from "effect/http-api/HttpApiSchema";

import {
  AuthAccessTokenResult,
  AuthAdministrativeScopes,
  AuthBrowserSessionRequest,
  AuthBrowserSessionResult,
  AuthCancelPairingOfferInput,
  AuthClientSession,
  AuthCreatePairingCredentialInput,
  AuthCreatePairingOfferInput,
  AuthEnvironmentBootstrapTokenType,
  AuthEnvironmentScope,
  AuthEnvironmentScopes,
  AuthAccessTokenType,
  AuthPairingCredentialResult,
  AuthPairingOfferResult,
  AuthShareStateResult,
  AuthPairingLink,
  AuthRevokeClientSessionInput,
  AuthRevokePairingLinkInput,
  AuthSessionState,
  AuthStandardClientScopes,
  AuthTokenExchangeGrantType,
  AuthTokenExchangeRequest,
  AuthWebSocketTicketResult,
} from "../src/auth.ts";
import {
  AuthClientSessionRevokeResult,
  AuthOtherClientSessionsRevokeResult,
  AuthPairingOfferCancellationResult,
  AuthPairingLinkRevokeResult,
  EnvironmentAuthInvalidError,
  EnvironmentHttpApi,
  EnvironmentInternalError,
  EnvironmentOperationForbiddenError,
  EnvironmentRequestInvalidError,
  EnvironmentScopeRequiredError,
} from "../src/environmentHttp.ts";

interface HttpApiSchemaIntrospection {
  readonly getResponseEncoding: (ast: SchemaAST.AST) => { readonly contentType: string };
  readonly getStatusError: (ast: SchemaAST.AST) => number;
}

// Effect uses these during HTTP reflection but omits them from its public declarations.
const HttpSchema = HttpApiSchema as typeof HttpApiSchema & HttpApiSchemaIntrospection;

const authRouteNames = [
  "session",
  "browserSession",
  "token",
  "webSocketTicket",
  "pairingCredential",
  "pairingOffer",
  "cancelPairingOffer",
  "shareState",
  "pairingLinks",
  "revokePairingLink",
  "clients",
  "revokeClient",
  "revokeOtherClients",
] as const;

interface SchemaShape {
  readonly status: number;
  readonly contentType: string;
  readonly schema: string;
  readonly fingerprint: string;
}

interface PayloadShape {
  readonly contentType: string;
  readonly schema: string;
  readonly fingerprint: string;
}

interface RouteManifest {
  readonly name: string;
  readonly method: string;
  readonly path: string;
  readonly requestContentTypes: ReadonlyArray<string>;
  readonly payloads: ReadonlyArray<PayloadShape>;
  readonly successes: ReadonlyArray<SchemaShape>;
  readonly errors: ReadonlyArray<SchemaShape>;
}

type RuntimeSchema = Schema.Codec<unknown, unknown, never, never>;
type RuntimeRoundTrip = (value: unknown) => unknown;

const roundTripCompilers = new WeakMap<Schema.Top, RuntimeRoundTrip>();
const compileRoundTrip = (codec: RuntimeSchema): RuntimeRoundTrip => {
  const decode = Schema.decodeUnknownSync(codec);
  const encode = Schema.encodeUnknownSync(codec);
  return (value) => encode(decode(value));
};

const fingerprintAst = (ast: SchemaAST.AST): string =>
  NodeCrypto.createHash("sha256").update(JSON.stringify(ast)).digest("hex");

const fingerprintSchema = (schema: Schema.Top): string => fingerprintAst(schema.ast);

const schemaName = (ast: SchemaAST.AST): string => {
  const identifier = ast.annotations?.identifier;
  return typeof identifier === "string" ? identifier : ast._tag;
};

const comparePayloadShapes = (left: PayloadShape, right: PayloadShape): number =>
  left.contentType.localeCompare(right.contentType) ||
  left.schema.localeCompare(right.schema) ||
  left.fingerprint.localeCompare(right.fingerprint);

const compareSchemaShapes = (left: SchemaShape, right: SchemaShape): number =>
  left.status - right.status ||
  left.contentType.localeCompare(right.contentType) ||
  left.schema.localeCompare(right.schema) ||
  left.fingerprint.localeCompare(right.fingerprint);

const payloadShapes = (payload: HttpApiEndpoint.PayloadMap): ReadonlyArray<PayloadShape> =>
  [...payload.entries()]
    .flatMap(([contentType, { schemas }]) =>
      schemas.map((schema) => ({
        contentType,
        schema: schemaName(schema.ast),
        fingerprint: fingerprintSchema(schema),
      })),
    )
    .toSorted(comparePayloadShapes);

const responseShapes = (
  responses: ReadonlyMap<number, readonly [Schema.Top, ...Array<Schema.Top>]>,
): ReadonlyArray<SchemaShape> => {
  const unique = new Map<string, SchemaShape>();
  for (const [status, schemas] of responses) {
    for (const schema of schemas) {
      const shape = {
        status,
        contentType: HttpSchema.getResponseEncoding(schema.ast).contentType,
        schema: schemaName(schema.ast),
        fingerprint: fingerprintSchema(schema),
      };
      unique.set(`${shape.status}:${shape.contentType}:${shape.fingerprint}`, shape);
    }
  }
  return [...unique.values()].toSorted(compareSchemaShapes);
};

const reflectedRoutes = new Map<string, RouteManifest>();
HttpApi.reflect(EnvironmentHttpApi, {
  onGroup() {},
  onEndpoint({ group, endpoint, successes, errors }) {
    if (group.identifier !== "auth") return;
    reflectedRoutes.set(endpoint.identifier, {
      name: endpoint.identifier,
      method: endpoint.method,
      path: endpoint.path,
      requestContentTypes: [...endpoint.payload.keys()].toSorted(),
      payloads: payloadShapes(endpoint.payload),
      successes: responseShapes(successes),
      errors: responseShapes(errors),
    });
  },
});

const unexpectedRoutes = [...reflectedRoutes.keys()].filter(
  (name) => !authRouteNames.includes(name as (typeof authRouteNames)[number]),
);
if (reflectedRoutes.size !== authRouteNames.length || unexpectedRoutes.length > 0) {
  throw new Error(`Unexpected auth routes: ${[...reflectedRoutes.keys()].join(", ")}.`);
}
const routes = authRouteNames.map((name) => {
  const route = reflectedRoutes.get(name);
  if (route === undefined) throw new Error(`Missing auth route ${name}.`);
  return route;
});

const AuthPairingLinkList = Schema.Array(AuthPairingLink);
const AuthClientSessionList = Schema.Array(AuthClientSession);

const namedSchemas = {
  AuthAccessTokenResult,
  AuthBrowserSessionRequest,
  AuthBrowserSessionResult,
  AuthClientSession,
  AuthClientSessionList,
  AuthClientSessionRevokeResult,
  AuthCancelPairingOfferInput,
  AuthCreatePairingCredentialInput,
  AuthCreatePairingOfferInput,
  AuthEnvironmentScope,
  AuthEnvironmentScopes,
  AuthOtherClientSessionsRevokeResult,
  AuthPairingCredentialResult,
  AuthPairingOfferResult,
  AuthPairingOfferCancellationResult,
  AuthShareStateResult,
  AuthPairingLink,
  AuthPairingLinkList,
  AuthPairingLinkRevokeResult,
  AuthRevokeClientSessionInput,
  AuthRevokePairingLinkInput,
  AuthSessionState,
  AuthTokenExchangeRequest,
  AuthWebSocketTicketResult,
  EnvironmentAuthInvalidError,
  EnvironmentInternalError,
  EnvironmentOperationForbiddenError,
  EnvironmentRequestInvalidError,
  EnvironmentScopeRequiredError,
} as const satisfies Readonly<Record<string, Schema.Top>>;

const schemaFingerprints = Object.fromEntries(
  Object.entries(namedSchemas)
    .map(([name, schema]) => [name, fingerprintSchema(schema)] as const)
    .toSorted(([left], [right]) => left.localeCompare(right)),
);

const stableErrors = [
  [EnvironmentRequestInvalidError, "errors/request-invalid.json"],
  [EnvironmentAuthInvalidError, "errors/auth-invalid.json"],
  [EnvironmentScopeRequiredError, "errors/scope-required.json"],
  [EnvironmentOperationForbiddenError, "errors/operation-forbidden.json"],
  [EnvironmentInternalError, "errors/internal.json"],
] as const;

const errors = stableErrors.map(([schema, fixture]) => ({
  schema: schemaName(schema.ast),
  status: HttpSchema.getStatusError(schema.ast),
  contentType: HttpSchema.getResponseEncoding(schema.ast).contentType,
  fingerprint: fingerprintSchema(schema),
  fixture,
}));

const roundTripEncoded = (schema: Schema.Top, value: unknown): unknown => {
  let roundTrip = roundTripCompilers.get(schema);
  if (roundTrip === undefined) {
    const codec = Schema.toCodecJson(schema as RuntimeSchema) as RuntimeSchema;
    roundTrip = compileRoundTrip(codec);
    roundTripCompilers.set(schema, roundTrip);
  }
  return roundTrip(value);
};

const jsonFixtures = new Map<string, unknown>();
const addJsonFixture = (path: string, schema: Schema.Top, value: unknown): unknown => {
  const encoded = roundTripEncoded(schema, value);
  jsonFixtures.set(path, encoded);
  return encoded;
};

const allScopes = [...AuthEnvironmentScope.literals];
const standardClientScopes = [...AuthStandardClientScopes];
const administrativeScopes = [...AuthAdministrativeScopes];
const issuedAt = "2026-01-01T00:00:00.000Z";
const lastConnectedAt = "2026-01-01T00:02:00.000Z";
const expiresAt = "2026-01-01T01:00:00.000Z";
const pairingExpiresAt = "2026-01-01T00:05:00.000Z";
const currentSessionId = "00000000-0000-4000-8000-000000000001";
const otherSessionId = "00000000-0000-4000-8000-000000000002";
const pairingId = "fixture-pairing-link";
const pairingCredential = "23456789ABCD";

addJsonFixture("responses/session.json", AuthSessionState, {
  authenticated: true,
  auth: {
    policy: "remote-reachable",
    bootstrapMethods: ["one-time-token"],
    sessionMethods: ["browser-session-cookie", "bearer-access-token", "dpop-access-token"],
    sessionCookieName: "coddy_studio_session",
  },
  scopes: administrativeScopes,
  sessionMethod: "browser-session-cookie",
  expiresAt,
});

addJsonFixture("requests/browser-session.json", AuthBrowserSessionRequest, {
  credential: "fixture-bootstrap-credential",
});
addJsonFixture("responses/browser-session.json", AuthBrowserSessionResult, {
  authenticated: true,
  scopes: standardClientScopes,
  sessionMethod: "browser-session-cookie",
  expiresAt,
});

const tokenRequest = addJsonFixture("token-request", AuthTokenExchangeRequest, {
  grant_type: AuthTokenExchangeGrantType,
  subject_token: "fixture-bootstrap-token",
  subject_token_type: AuthEnvironmentBootstrapTokenType,
  requested_token_type: AuthAccessTokenType,
  scope: standardClientScopes.join(" "),
  client_label: "Fixture CLI",
  client_device_type: "bot",
  client_os: "FixtureOS 1.0",
});
jsonFixtures.delete("token-request");
if (tokenRequest === null || typeof tokenRequest !== "object" || Array.isArray(tokenRequest)) {
  throw new Error("Encoded token request was not a form record.");
}
const tokenForm = new URLSearchParams(
  Object.entries(tokenRequest).map(([key, value]) => [key, String(value)]),
).toString();
addJsonFixture("responses/token.json", AuthAccessTokenResult, {
  access_token: "fixture.dpop.access-token",
  issued_token_type: AuthAccessTokenType,
  token_type: "DPoP",
  expires_in: 3600,
  scope: standardClientScopes.join(" "),
});

addJsonFixture("responses/websocket-ticket.json", AuthWebSocketTicketResult, {
  ticket: "fixture.websocket.ticket",
  expiresAt: pairingExpiresAt,
});

addJsonFixture("requests/pairing-create.json", AuthCreatePairingCredentialInput, {
  label: "Fixture pairing link",
  scopes: standardClientScopes,
});
addJsonFixture("responses/pairing-create.json", AuthPairingCredentialResult, {
  id: pairingId,
  credential: pairingCredential,
  label: "Fixture pairing link",
  expiresAt: pairingExpiresAt,
});
addJsonFixture("requests/pairing-offer.json", AuthCreatePairingOfferInput, {
  name: "AI-SERVER",
  endpoint: "http://192.168.1.20:3773",
  reach: "another-device",
  label: "Tablet",
});
addJsonFixture("responses/pairing-offer.json", AuthPairingOfferResult, {
  id: "4f8f2f2e-0000-4000-8000-000000000000",
  code: "eyJ2IjoxfQ",
  reach: "another-device",
  endpoint: "http://192.168.1.20:3773",
  name: "AI-SERVER",
  expiresAt: "2026-08-27T01:00:00.000Z",
});
addJsonFixture("requests/pairing-offer-cancel.json", AuthCancelPairingOfferInput, {
  idempotencyKey: "fixture-pairing-offer-key",
});
addJsonFixture("responses/pairing-offer-cancel.json", AuthPairingOfferCancellationResult, {
  cancelled: true,
});
addJsonFixture("responses/share-state.json", AuthShareStateResult, {
  desiredExposure: "loopback",
  offHostGrantCount: 0,
  legacyGrantCount: 1,
});
addJsonFixture("responses/pairing-list.json", AuthPairingLinkList, [
  {
    id: pairingId,
    credential: pairingCredential,
    scopes: standardClientScopes,
    subject: "fixture-client",
    label: "Fixture pairing link",
    reach: "another-device",
    createdAt: issuedAt,
    expiresAt: pairingExpiresAt,
  },
]);
addJsonFixture("requests/pairing-revoke.json", AuthRevokePairingLinkInput, {
  id: pairingId,
});
addJsonFixture("responses/pairing-revoke.json", AuthPairingLinkRevokeResult, {
  revoked: true,
});

addJsonFixture("responses/client-list.json", AuthClientSessionList, [
  {
    sessionId: currentSessionId,
    subject: "fixture-current-client",
    scopes: administrativeScopes,
    method: "browser-session-cookie",
    client: {
      label: "Fixture browser",
      ipAddress: "127.0.0.1",
      userAgent: "FixtureBrowser/1.0",
      deviceType: "desktop",
      os: "FixtureOS 1.0",
      browser: "FixtureBrowser",
    },
    issuedAt,
    expiresAt,
    lastConnectedAt,
    connected: true,
    current: true,
  },
  {
    sessionId: otherSessionId,
    subject: "fixture-other-client",
    scopes: standardClientScopes,
    method: "dpop-access-token",
    client: {
      label: "Fixture automation",
      deviceType: "bot",
    },
    issuedAt,
    expiresAt,
    lastConnectedAt: null,
    connected: false,
    current: false,
    reach: "another-device",
  },
]);
addJsonFixture("requests/client-revoke.json", AuthRevokeClientSessionInput, {
  sessionId: otherSessionId,
});
addJsonFixture("responses/client-revoke.json", AuthClientSessionRevokeResult, {
  revoked: true,
});
addJsonFixture("responses/client-revoke-others.json", AuthOtherClientSessionsRevokeResult, {
  revokedCount: 1,
});

addJsonFixture("errors/request-invalid.json", EnvironmentRequestInvalidError, {
  _tag: "EnvironmentRequestInvalidError",
  code: "invalid_request",
  reason: "invalid_scope",
  traceId: "fixture-trace-request-invalid",
});
addJsonFixture("errors/auth-invalid.json", EnvironmentAuthInvalidError, {
  _tag: "EnvironmentAuthInvalidError",
  code: "auth_invalid",
  reason: "missing_credential",
  traceId: "fixture-trace-auth-invalid",
});
addJsonFixture("errors/scope-required.json", EnvironmentScopeRequiredError, {
  _tag: "EnvironmentScopeRequiredError",
  code: "insufficient_scope",
  requiredScope: "access:write",
  traceId: "fixture-trace-scope-required",
});
addJsonFixture("errors/operation-forbidden.json", EnvironmentOperationForbiddenError, {
  _tag: "EnvironmentOperationForbiddenError",
  code: "operation_forbidden",
  reason: "current_session_revoke_not_allowed",
  traceId: "fixture-trace-operation-forbidden",
});
addJsonFixture("errors/internal.json", EnvironmentInternalError, {
  _tag: "EnvironmentInternalError",
  code: "internal_error",
  reason: "internal_error",
  traceId: "fixture-trace-internal",
});

jsonFixtures.set("scopes.json", {
  all: roundTripEncoded(AuthEnvironmentScopes, allScopes),
  standardClient: roundTripEncoded(AuthEnvironmentScopes, standardClientScopes),
  administrative: roundTripEncoded(AuthEnvironmentScopes, administrativeScopes),
});

const samples = {
  session: { success: "responses/session.json" },
  browserSession: {
    request: "requests/browser-session.json",
    success: "responses/browser-session.json",
  },
  token: {
    request: "requests/token-form.txt",
    success: "responses/token.json",
  },
  webSocketTicket: { success: "responses/websocket-ticket.json" },
  pairingCredential: {
    request: "requests/pairing-create.json",
    success: "responses/pairing-create.json",
  },
  pairingOffer: {
    request: "requests/pairing-offer.json",
    success: "responses/pairing-offer.json",
  },
  cancelPairingOffer: {
    request: "requests/pairing-offer-cancel.json",
    success: "responses/pairing-offer-cancel.json",
  },
  shareState: { success: "responses/share-state.json" },
  pairingLinks: { success: "responses/pairing-list.json" },
  revokePairingLink: {
    request: "requests/pairing-revoke.json",
    success: "responses/pairing-revoke.json",
  },
  clients: { success: "responses/client-list.json" },
  revokeClient: {
    request: "requests/client-revoke.json",
    success: "responses/client-revoke.json",
  },
  revokeOtherClients: { success: "responses/client-revoke-others.json" },
} as const;

const textFixtures = new Map<string, string>([["requests/token-form.txt", tokenForm]]);
const fixtures = [...jsonFixtures.keys(), ...textFixtures.keys()].toSorted();
const manifest = {
  formatVersion: 1,
  routes,
  errors,
  scopes: {
    all: allScopes,
    standardClient: standardClientScopes,
    administrative: administrativeScopes,
  },
  samples,
  schemaFingerprints,
  fixtures,
};

const outputDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/auth-http");
await NodeFSP.rm(outputDirectory, { force: true, recursive: true });
await NodeFSP.mkdir(outputDirectory, { recursive: true });

for (const [relativePath, fixture] of [...jsonFixtures.entries()].toSorted(([left], [right]) =>
  left.localeCompare(right),
)) {
  const outputPath = NodePath.join(outputDirectory, relativePath);
  await NodeFSP.mkdir(NodePath.dirname(outputPath), { recursive: true });
  await NodeFSP.writeFile(outputPath, `${JSON.stringify(fixture, null, 2)}\n`);
}
for (const [relativePath, fixture] of [...textFixtures.entries()].toSorted(([left], [right]) =>
  left.localeCompare(right),
)) {
  const outputPath = NodePath.join(outputDirectory, relativePath);
  await NodeFSP.mkdir(NodePath.dirname(outputPath), { recursive: true });
  await NodeFSP.writeFile(outputPath, `${fixture}\n`);
}
await NodeFSP.writeFile(
  NodePath.join(outputDirectory, "manifest.json"),
  `${JSON.stringify(manifest, null, 2)}\n`,
);

const formatResult = NodeChildProcess.spawnSync("vp", ["fmt", "--write", outputDirectory], {
  cwd: NodePath.resolve(import.meta.dirname, "../../.."),
  stdio: "inherit",
});
if (formatResult.status !== 0) {
  throw new Error(
    `Failed to format auth HTTP fixtures (exit ${formatResult.status ?? "unknown"}).`,
  );
}
