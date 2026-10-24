// @effect-diagnostics nodeBuiltinImport:off
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { describe, expect, it } from "@effect/vitest";
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
  AuthEnvironmentScope,
  AuthEnvironmentScopes,
  AuthPairingCredentialResult,
  AuthPairingOfferResult,
  AuthShareStateResult,
  AuthPairingLink,
  AuthRevokeClientSessionInput,
  AuthRevokePairingLinkInput,
  AuthSessionState,
  AuthStandardClientScopes,
  AuthTokenExchangeRequest,
  AuthWebSocketTicketResult,
} from "./auth.ts";
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
} from "./environmentHttp.ts";

interface HttpApiSchemaIntrospection {
  readonly getResponseEncoding: (ast: SchemaAST.AST) => { readonly contentType: string };
  readonly getStatusError: (ast: SchemaAST.AST) => number;
}

// Effect uses these during HTTP reflection but omits them from its public declarations.
const HttpSchema = HttpApiSchema as typeof HttpApiSchema & HttpApiSchemaIntrospection;

const authRouteContract = [
  {
    name: "session",
    method: "GET",
    path: "/api/auth/session",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [500],
  },
  {
    name: "browserSession",
    method: "POST",
    path: "/api/auth/browser-session",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [401, 500],
  },
  {
    name: "token",
    method: "POST",
    path: "/oauth/token",
    requestContentTypes: ["application/x-www-form-urlencoded"],
    successStatuses: [200],
    errorStatuses: [400, 401, 500],
  },
  {
    name: "webSocketTicket",
    method: "POST",
    path: "/api/auth/websocket-ticket",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "pairingCredential",
    method: "POST",
    path: "/api/auth/pairing-token",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [400, 401, 403, 500],
  },
  {
    name: "pairingOffer",
    method: "POST",
    path: "/api/auth/pairing-offer",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [400, 401, 403, 500],
  },
  {
    name: "cancelPairingOffer",
    method: "POST",
    path: "/api/auth/pairing-offer/cancel",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [400, 401, 403, 500],
  },
  {
    name: "shareState",
    method: "GET",
    path: "/api/auth/share-state",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "pairingLinks",
    method: "GET",
    path: "/api/auth/pairing-links",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "revokePairingLink",
    method: "POST",
    path: "/api/auth/pairing-links/revoke",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "clients",
    method: "GET",
    path: "/api/auth/clients",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "revokeClient",
    method: "POST",
    path: "/api/auth/clients/revoke",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
  {
    name: "revokeOtherClients",
    method: "POST",
    path: "/api/auth/clients/revoke-others",
    requestContentTypes: [],
    successStatuses: [200],
    errorStatuses: [401, 403, 500],
  },
] as const;

const authRouteNames = authRouteContract.map(({ name }) => name);

const scopeContract = {
  all: [
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
    "review:write",
    "access:read",
    "access:write",
    "relay:read",
    "relay:write",
  ],
  standardClient: [
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
    "review:write",
    "relay:read",
  ],
  administrative: [
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
    "review:write",
    "relay:read",
    "access:read",
    "access:write",
    "relay:write",
  ],
} as const;

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

interface ErrorManifest {
  readonly schema: string;
  readonly status: number;
  readonly contentType: string;
  readonly fingerprint: string;
  readonly fixture: string;
}

interface Manifest {
  readonly formatVersion: 1;
  readonly routes: ReadonlyArray<RouteManifest>;
  readonly errors: ReadonlyArray<ErrorManifest>;
  readonly scopes: {
    readonly all: ReadonlyArray<string>;
    readonly standardClient: ReadonlyArray<string>;
    readonly administrative: ReadonlyArray<string>;
  };
  readonly samples: Readonly<Record<string, Readonly<Record<string, string>>>>;
  readonly schemaFingerprints: Readonly<Record<string, string>>;
  readonly fixtures: ReadonlyArray<string>;
}

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

const currentRoutes = (): ReadonlyArray<RouteManifest> => {
  const routes = new Map<string, RouteManifest>();
  HttpApi.reflect(EnvironmentHttpApi, {
    onGroup() {},
    onEndpoint({ group, endpoint, successes, errors }) {
      if (group.identifier !== "auth") return;
      routes.set(endpoint.identifier, {
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

  const unexpectedRoutes = [...routes.keys()].filter(
    (name) => !authRouteNames.includes(name as (typeof authRouteNames)[number]),
  );
  if (routes.size !== authRouteNames.length || unexpectedRoutes.length > 0) {
    throw new Error(`Unexpected auth routes: ${[...routes.keys()].join(", ")}.`);
  }

  return authRouteNames.map((name) => {
    const route = routes.get(name);
    if (route === undefined) throw new Error(`Missing auth route ${name}.`);
    return route;
  });
};

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

const currentSchemaFingerprints = (): Readonly<Record<string, string>> =>
  Object.fromEntries(
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

const currentErrors = (): ReadonlyArray<ErrorManifest> =>
  stableErrors.map(([schema, fixture]) => ({
    schema: schemaName(schema.ast),
    status: HttpSchema.getStatusError(schema.ast),
    contentType: HttpSchema.getResponseEncoding(schema.ast).contentType,
    fingerprint: fingerprintSchema(schema),
    fixture,
  }));

const expectedSamples = {
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

const expectedFixtures = [
  "errors/auth-invalid.json",
  "errors/internal.json",
  "errors/operation-forbidden.json",
  "errors/request-invalid.json",
  "errors/scope-required.json",
  "requests/browser-session.json",
  "requests/client-revoke.json",
  "requests/pairing-create.json",
  "requests/pairing-offer-cancel.json",
  "requests/pairing-offer.json",
  "requests/pairing-revoke.json",
  "requests/token-form.txt",
  "responses/browser-session.json",
  "responses/client-list.json",
  "responses/client-revoke-others.json",
  "responses/client-revoke.json",
  "responses/pairing-create.json",
  "responses/pairing-list.json",
  "responses/pairing-offer-cancel.json",
  "responses/pairing-offer.json",
  "responses/pairing-revoke.json",
  "responses/session.json",
  "responses/share-state.json",
  "responses/token.json",
  "responses/websocket-ticket.json",
  "scopes.json",
] as const;

const fixtureDecoders = new Map<string, (value: unknown) => unknown>([
  [
    "requests/browser-session.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthBrowserSessionRequest)),
  ],
  [
    "requests/client-revoke.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthRevokeClientSessionInput)),
  ],
  [
    "requests/pairing-create.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthCreatePairingCredentialInput)),
  ],
  [
    "requests/pairing-offer.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthCreatePairingOfferInput)),
  ],
  [
    "requests/pairing-offer-cancel.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthCancelPairingOfferInput)),
  ],
  [
    "requests/pairing-revoke.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthRevokePairingLinkInput)),
  ],
  [
    "responses/browser-session.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthBrowserSessionResult)),
  ],
  [
    "responses/client-list.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthClientSessionList)),
  ],
  [
    "responses/client-revoke-others.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthOtherClientSessionsRevokeResult)),
  ],
  [
    "responses/client-revoke.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthClientSessionRevokeResult)),
  ],
  [
    "responses/pairing-create.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingCredentialResult)),
  ],
  [
    "responses/pairing-offer.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingOfferResult)),
  ],
  [
    "responses/pairing-offer-cancel.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingOfferCancellationResult)),
  ],
  [
    "responses/pairing-list.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingLinkList)),
  ],
  [
    "responses/pairing-revoke.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingLinkRevokeResult)),
  ],
  ["responses/session.json", Schema.decodeUnknownSync(Schema.toCodecJson(AuthSessionState))],
  [
    "responses/share-state.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthShareStateResult)),
  ],
  ["responses/token.json", Schema.decodeUnknownSync(Schema.toCodecJson(AuthAccessTokenResult))],
  [
    "responses/websocket-ticket.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(AuthWebSocketTicketResult)),
  ],
  [
    "errors/request-invalid.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(EnvironmentRequestInvalidError)),
  ],
  [
    "errors/auth-invalid.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(EnvironmentAuthInvalidError)),
  ],
  [
    "errors/scope-required.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(EnvironmentScopeRequiredError)),
  ],
  [
    "errors/operation-forbidden.json",
    Schema.decodeUnknownSync(Schema.toCodecJson(EnvironmentOperationForbiddenError)),
  ],
  ["errors/internal.json", Schema.decodeUnknownSync(Schema.toCodecJson(EnvironmentInternalError))],
]);

const decodeTokenForm = Schema.decodeUnknownSync(Schema.toCodecJson(AuthTokenExchangeRequest));
const decodeScopes = Schema.decodeUnknownSync(Schema.toCodecJson(AuthEnvironmentScopes));

const listRelativeFiles = (directory: string, prefix = ""): ReadonlyArray<string> =>
  NodeFS.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const relativePath = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
    return entry.isDirectory()
      ? listRelativeFiles(NodePath.join(directory, entry.name), relativePath)
      : [relativePath];
  });

describe("Rust auth HTTP fixture parity", () => {
  it("tracks executable auth routes and schemas without missing or stale fixtures", () => {
    const fixtureDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/auth-http");
    const manifestPath = NodePath.join(fixtureDirectory, "manifest.json");
    const manifestExists = NodeFS.existsSync(manifestPath);
    expect(manifestExists).toBe(true);
    if (!manifestExists) return;

    const manifest = JSON.parse(NodeFS.readFileSync(manifestPath, "utf8")) as Manifest;
    expect(manifest.formatVersion).toBe(1);
    expect(manifest.routes).toEqual(currentRoutes());
    expect(manifest.routes).toHaveLength(13);
    expect(
      manifest.routes.map(({ name, method, path, requestContentTypes, successes, errors }) => ({
        name,
        method,
        path,
        requestContentTypes,
        successStatuses: [...new Set(successes.map(({ status }) => status))].toSorted(),
        errorStatuses: [...new Set(errors.map(({ status }) => status))].toSorted(),
      })),
    ).toEqual(authRouteContract);
    expect(manifest.errors).toEqual(currentErrors());
    expect(
      manifest.errors.map(({ schema, status, contentType }) => ({
        schema,
        status,
        contentType,
      })),
    ).toEqual([
      {
        schema: "EnvironmentRequestInvalidError",
        status: 400,
        contentType: "application/json",
      },
      {
        schema: "EnvironmentAuthInvalidError",
        status: 401,
        contentType: "application/json",
      },
      {
        schema: "EnvironmentScopeRequiredError",
        status: 403,
        contentType: "application/json",
      },
      {
        schema: "EnvironmentOperationForbiddenError",
        status: 403,
        contentType: "application/json",
      },
      {
        schema: "EnvironmentInternalError",
        status: 500,
        contentType: "application/json",
      },
    ]);
    const executableScopes = {
      all: [...AuthEnvironmentScope.literals],
      standardClient: [...AuthStandardClientScopes],
      administrative: [...AuthAdministrativeScopes],
    };
    expect(executableScopes).toEqual(scopeContract);
    expect(manifest.scopes).toEqual(scopeContract);
    expect(manifest.samples).toEqual(expectedSamples);
    expect(manifest.schemaFingerprints).toEqual(currentSchemaFingerprints());
    expect(manifest.fixtures).toEqual(expectedFixtures);
    expect(new Set(manifest.fixtures).size).toBe(manifest.fixtures.length);

    for (const fixture of manifest.fixtures) {
      expect(NodeFS.existsSync(NodePath.join(fixtureDirectory, fixture))).toBe(true);
    }
    expect(listRelativeFiles(fixtureDirectory).toSorted()).toEqual(
      ["manifest.json", ...manifest.fixtures].toSorted(),
    );

    for (const [fixture, decode] of fixtureDecoders) {
      const encoded = JSON.parse(
        NodeFS.readFileSync(NodePath.join(fixtureDirectory, fixture), "utf8"),
      ) as unknown;
      decode(encoded);
    }

    const tokenForm = Object.fromEntries(
      new URLSearchParams(
        NodeFS.readFileSync(
          NodePath.join(fixtureDirectory, "requests/token-form.txt"),
          "utf8",
        ).trim(),
      ),
    );
    decodeTokenForm(tokenForm);

    const scopes = JSON.parse(
      NodeFS.readFileSync(NodePath.join(fixtureDirectory, "scopes.json"), "utf8"),
    ) as {
      readonly all: unknown;
      readonly standardClient: unknown;
      readonly administrative: unknown;
    };
    decodeScopes(scopes.all);
    decodeScopes(scopes.standardClient);
    decodeScopes(scopes.administrative);
  });
});
