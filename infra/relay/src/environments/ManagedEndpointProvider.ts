import * as Alchemy from "alchemy";
import * as Cloudflare from "alchemy/Cloudflare";
import * as Arr from "effect/Array";
import * as Context from "effect/Context";
import * as Crypto from "effect/Crypto";
import * as Effect from "effect/Effect";
import * as Hex from "effect/encoding/Hex";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import type {
  RelayManagedEndpoint,
  RelayManagedEndpointOrigin,
  RelayManagedEndpointRuntimeConfig,
} from "@bibcode/contracts/relay";

import * as RelayConfiguration from "../Config.ts";
import {
  managedEndpointDigestInput,
  managedEndpointForHostname,
  managedEndpointHostname,
  managedEndpointPreviewHostnames,
  managedEndpointTunnelName,
} from "../deploymentConfig.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";

export class ManagedEndpointProvisioningNotConfigured extends Schema.TaggedError<ManagedEndpointProvisioningNotConfigured>()(
  "ManagedEndpointProvisioningNotConfigured",
  {
    userId: Schema.String,
    environmentId: Schema.String,
    missingSettings: Schema.Array(
      Schema.Literals(["managedEndpointBaseDomain", "managedEndpointNamespace"]),
    ),
  },
) {
  override get message(): string {
    return `Managed endpoint provisioning is not configured for user '${this.userId}', environment '${this.environmentId}': missing ${this.missingSettings.join(", ")}`;
  }
}

const ManagedEndpointProvisioningStage = Schema.Literals([
  "derive-environment-hash",
  "reserve-allocation",
  "ensure-tunnel",
  "validate-tunnel-response",
  "record-tunnel",
  "configure-tunnel",
  "ensure-dns-record",
  "record-dns",
  "ensure-preview-dns-record",
  "get-tunnel-token",
  "mark-allocation-ready",
]);

export class ManagedEndpointProvisioningFailed extends Schema.TaggedError<ManagedEndpointProvisioningFailed>()(
  "ManagedEndpointProvisioningFailed",
  {
    stage: ManagedEndpointProvisioningStage,
    userId: Schema.String,
    environmentId: Schema.String,
    hostname: Schema.optionalKey(Schema.String),
    tunnelName: Schema.optionalKey(Schema.String),
    tunnelId: Schema.optionalKey(Schema.String),
    dnsRecordId: Schema.optionalKey(Schema.String),
    returnedTunnelName: Schema.optionalKey(Schema.String),
    returnedTunnelId: Schema.optionalKey(Schema.String),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return `Managed endpoint provisioning failed during '${this.stage}' for user '${this.userId}', environment '${this.environmentId}'`;
  }
}

const ManagedEndpointDeprovisioningStage = Schema.Literals([
  "load-allocation",
  "delete-preview-dns-record",
  "delete-dns-record",
  "delete-tunnel",
  "remove-allocation",
]);

export class ManagedEndpointDeprovisioningFailed extends Schema.TaggedError<ManagedEndpointDeprovisioningFailed>()(
  "ManagedEndpointDeprovisioningFailed",
  {
    stage: ManagedEndpointDeprovisioningStage,
    userId: Schema.String,
    environmentId: Schema.String,
    tunnelId: Schema.optionalKey(Schema.String),
    dnsRecordId: Schema.optionalKey(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Managed endpoint deprovisioning failed during '${this.stage}' for user '${this.userId}', environment '${this.environmentId}'`;
  }
}

export class ManagedEndpointOriginNotAllowed extends Schema.TaggedError<ManagedEndpointOriginNotAllowed>()(
  "ManagedEndpointOriginNotAllowed",
  {
    userId: Schema.String,
    environmentId: Schema.String,
    host: Schema.String,
    port: Schema.Finite,
  },
) {
  override get message(): string {
    return `Managed endpoint origin '${this.host}:${this.port}' is not allowed for user '${this.userId}', environment '${this.environmentId}'`;
  }
}

export class ManagedEndpointProviderCallTimedOut extends Schema.TaggedError<ManagedEndpointProviderCallTimedOut>()(
  "ManagedEndpointProviderCallTimedOut",
  { timeoutMillis: Schema.Finite },
) {
  override get message(): string {
    return `Managed endpoint provider call exceeded ${this.timeoutMillis}ms`;
  }
}

export type ManagedEndpointProviderError =
  | ManagedEndpointProvisioningNotConfigured
  | ManagedEndpointProvisioningFailed
  | ManagedEndpointOriginNotAllowed;

export interface ManagedEndpointProvisioningResult {
  readonly endpoint: RelayManagedEndpoint;
  readonly runtime: RelayManagedEndpointRuntimeConfig;
  readonly endpointDisposition: "created" | "reused";
}

interface ManagedEndpointProvisionInput {
  readonly userId: string;
  readonly environmentId: string;
  readonly origin: RelayManagedEndpointOrigin;
  readonly ownership?: ManagedEndpointAllocations.ManagedEndpointOperation;
}

interface ManagedEndpointDeprovisionInput {
  readonly userId: string;
  readonly environmentId: string;
  readonly ownership?: ManagedEndpointAllocations.ManagedEndpointOperation;
}

export class ManagedEndpointProvider extends Context.Service<
  ManagedEndpointProvider,
  {
    readonly provision: (
      input: ManagedEndpointProvisionInput,
    ) => Effect.Effect<ManagedEndpointProvisioningResult, ManagedEndpointProviderError>;
    readonly deprovision: (
      input: ManagedEndpointDeprovisionInput,
    ) => Effect.Effect<void, ManagedEndpointDeprovisioningFailed>;
  }
>()("bibcode-relay/environments/ManagedEndpointProvider") {}

interface ManagedEndpointTunnel {
  readonly id?: string | null;
  readonly name?: string | null;
}

const ManagedEndpointTunnelClientOperation = Schema.Literals([
  "list",
  "create",
  "put-configuration",
  "get-token",
  "delete",
]);

export class ManagedEndpointTunnelClientError extends Schema.TaggedError<ManagedEndpointTunnelClientError>()(
  "ManagedEndpointTunnelClientError",
  {
    operation: ManagedEndpointTunnelClientOperation,
    tunnelName: Schema.optionalKey(Schema.String),
    tunnelId: Schema.optionalKey(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    const target = this.tunnelId ?? this.tunnelName;
    return `Managed endpoint tunnel provider '${this.operation}' request failed${target === undefined ? "" : ` for '${target}'`}`;
  }
}

export class ManagedEndpointTunnelClient extends Context.Service<
  ManagedEndpointTunnelClient,
  {
    readonly list: (request: {
      readonly name: string;
      readonly isDeleted: false;
    }) => Effect.Effect<
      { readonly result: ReadonlyArray<ManagedEndpointTunnel> },
      ManagedEndpointTunnelClientError
    >;
    readonly create: (request: {
      readonly name: string;
      readonly configSrc: "cloudflare";
    }) => Effect.Effect<ManagedEndpointTunnel, ManagedEndpointTunnelClientError>;
    readonly putConfiguration: (
      tunnelId: string,
      config: {
        readonly ingress: Array<{
          readonly hostname?: string;
          readonly service: string;
        }>;
      },
    ) => Effect.Effect<unknown, ManagedEndpointTunnelClientError>;
    readonly getToken: (
      tunnelId: string,
    ) => Effect.Effect<string, ManagedEndpointTunnelClientError>;
    readonly delete: (tunnelId: string) => Effect.Effect<unknown, ManagedEndpointTunnelClientError>;
  }
>()("bibcode-relay/environments/ManagedEndpointProvider/ManagedEndpointTunnelClient") {}

export const layerTunnelClient = (client: ManagedEndpointTunnelClient["Service"]) =>
  Layer.succeed(ManagedEndpointTunnelClient, client);

interface ManagedEndpointCnameRecordInput {
  readonly type: "CNAME";
  readonly name: string;
  readonly content: string;
  readonly ttl: 1;
  readonly proxied: true;
}

const ManagedEndpointDnsClientOperation = Schema.Literals([
  "list-records",
  "create-record",
  "update-record",
  "delete-record",
]);

export class ManagedEndpointDnsClientError extends Schema.TaggedError<ManagedEndpointDnsClientError>()(
  "ManagedEndpointDnsClientError",
  {
    operation: ManagedEndpointDnsClientOperation,
    hostname: Schema.optionalKey(Schema.String),
    dnsRecordId: Schema.optionalKey(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    const target = this.dnsRecordId ?? this.hostname;
    return `Managed endpoint DNS provider '${this.operation}' request failed${target === undefined ? "" : ` for '${target}'`}`;
  }
}

export class ManagedEndpointDnsClient extends Context.Service<
  ManagedEndpointDnsClient,
  {
    readonly listRecords: (
      hostname: string,
    ) => Effect.Effect<ReadonlyArray<{ readonly id: string }>, ManagedEndpointDnsClientError>;
    readonly createRecord: (
      request: ManagedEndpointCnameRecordInput,
    ) => Effect.Effect<{ readonly id: string }, ManagedEndpointDnsClientError>;
    readonly updateRecord: (
      dnsRecordId: string,
      request: ManagedEndpointCnameRecordInput,
    ) => Effect.Effect<unknown, ManagedEndpointDnsClientError>;
    readonly deleteRecord: (
      dnsRecordId: string,
    ) => Effect.Effect<unknown, ManagedEndpointDnsClientError>;
  }
>()("bibcode-relay/environments/ManagedEndpointProvider/ManagedEndpointDnsClient") {}

export const layerDnsClient = (client: ManagedEndpointDnsClient["Service"]) =>
  Layer.succeed(ManagedEndpointDnsClient, client);

const requireCloudflareSettings = Effect.fnUntraced(function* (
  settings: RelayConfiguration.RelayConfiguration["Service"],
  input: { readonly userId: string; readonly environmentId: string },
) {
  const baseDomain = settings.managedEndpointBaseDomain;
  const namespace = settings.managedEndpointNamespace;
  const missingSettings: Array<"managedEndpointBaseDomain" | "managedEndpointNamespace"> = [];
  if (!baseDomain) {
    missingSettings.push("managedEndpointBaseDomain");
  }
  if (!namespace) {
    missingSettings.push("managedEndpointNamespace");
  }
  if (!baseDomain || !namespace) {
    return yield* new ManagedEndpointProvisioningNotConfigured({
      ...input,
      missingSettings,
    });
  }
  return {
    baseDomain,
    namespace,
  };
});

function formatOriginService(origin: RelayManagedEndpointOrigin): string {
  const host = origin.localHttpHost.includes(":")
    ? `[${origin.localHttpHost.replace(/^\[(.*)\]$/u, "$1")}]`
    : origin.localHttpHost;
  return `http://${host}:${origin.localHttpPort}`;
}

function normalizeHostname(hostname: string): string {
  return hostname
    .trim()
    .toLowerCase()
    .replace(/\.$/u, "")
    .replace(/^\[(.*)\]$/u, "$1");
}

function isLoopbackOrigin(origin: RelayManagedEndpointOrigin): boolean {
  const hostname = normalizeHostname(origin.localHttpHost);
  return (
    (hostname === "127.0.0.1" || hostname === "::1" || hostname === "localhost") &&
    Number.isInteger(origin.localHttpPort) &&
    origin.localHttpPort > 0 &&
    origin.localHttpPort <= 65_535
  );
}

function isNotFoundCause(cause: unknown): boolean {
  if (typeof cause !== "object" || cause === null) {
    return false;
  }
  if ("_tag" in cause && cause._tag === "NotFound") {
    return true;
  }
  if ("status" in cause && cause.status === 404) {
    return true;
  }
  return "cause" in cause && isNotFoundCause(cause.cause);
}

function redactProviderCause(cause: unknown): { readonly _tag: "NotFound" | "ProviderError" } {
  return { _tag: isNotFoundCause(cause) ? "NotFound" : "ProviderError" };
}

type ManagedEndpointClientError =
  | ManagedEndpointTunnelClientError
  | ManagedEndpointDnsClientError
  | ManagedEndpointProviderCallTimedOut;

const PROVIDER_CALL_TIMEOUT_SAFETY_MARGIN_MILLIS = 60 * 1_000;
export const MANAGED_ENDPOINT_PROVIDER_CALL_TIMEOUT_MILLIS =
  ManagedEndpointAllocations.MANAGED_ENDPOINT_OPERATION_LEASE_MILLIS -
  PROVIDER_CALL_TIMEOUT_SAFETY_MARGIN_MILLIS;

const ignoreNotFound = <A>(
  effect: Effect.Effect<A, ManagedEndpointClientError>,
): Effect.Effect<void, ManagedEndpointClientError> =>
  effect.pipe(
    Effect.asVoid,
    Effect.catchTags({
      ManagedEndpointTunnelClientError: (error) =>
        isNotFoundCause(error.cause) ? Effect.void : Effect.fail(error),
      ManagedEndpointDnsClientError: (error) =>
        isNotFoundCause(error.cause) ? Effect.void : Effect.fail(error),
    }),
  );

export const make = Effect.gen(function* () {
  const config = yield* RelayConfiguration.RelayConfiguration;
  const crypto = yield* Crypto.Crypto;
  const tunnels = yield* ManagedEndpointTunnelClient;
  const dns = yield* ManagedEndpointDnsClient;
  const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;

  const withOperationFence = <A, E, R>(
    ownership: ManagedEndpointAllocations.ManagedEndpointOperation,
    effect: Effect.Effect<A, E, R>,
  ) =>
    Effect.acquireUseRelease(
      allocations.renewOperation(ownership),
      () =>
        effect.pipe(
          Effect.timeoutOrElse({
            duration: MANAGED_ENDPOINT_PROVIDER_CALL_TIMEOUT_MILLIS,
            orElse: () =>
              Effect.fail(
                new ManagedEndpointProviderCallTimedOut({
                  timeoutMillis: MANAGED_ENDPOINT_PROVIDER_CALL_TIMEOUT_MILLIS,
                }),
              ),
          }),
        ),
      () => allocations.renewOperation(ownership),
    );

  const updateExistingDnsRecords = Effect.fnUntraced(function* (
    ownership: ManagedEndpointAllocations.ManagedEndpointOperation,
    records: ReadonlyArray<{ readonly id: string }>,
    preferredDnsRecordId: string | null,
    dnsRecord: ManagedEndpointCnameRecordInput,
  ) {
    const keptRecord = records.find((record) => record.id === preferredDnsRecordId) ?? records[0];
    if (keptRecord === undefined) {
      return null;
    }
    yield* Effect.forEach(
      records,
      (record) =>
        record.id === keptRecord.id
          ? Effect.void
          : withOperationFence(ownership, dns.deleteRecord(record.id)),
      { discard: true },
    );
    yield* withOperationFence(ownership, dns.updateRecord(keptRecord.id, dnsRecord));
    return keptRecord.id;
  });

  const ensureDnsRecord = Effect.fnUntraced(function* (
    ownership: ManagedEndpointAllocations.ManagedEndpointOperation,
    hostname: string,
    preferredDnsRecordId: string | null,
    dnsRecord: ManagedEndpointCnameRecordInput,
  ) {
    if (preferredDnsRecordId !== null) {
      const checkpointedRecordUpdated = yield* withOperationFence(
        ownership,
        dns.updateRecord(preferredDnsRecordId, dnsRecord),
      ).pipe(
        Effect.as(true),
        Effect.catchTags({
          ManagedEndpointDnsClientError: (error) =>
            isNotFoundCause(error.cause) ? Effect.succeed(false) : Effect.fail(error),
        }),
      );
      if (checkpointedRecordUpdated) {
        return preferredDnsRecordId;
      }
    }
    const existingDnsRecords = yield* withOperationFence(ownership, dns.listRecords(hostname));
    const existingDnsRecordId = yield* updateExistingDnsRecords(
      ownership,
      existingDnsRecords,
      preferredDnsRecordId,
      dnsRecord,
    );
    if (existingDnsRecordId !== null) {
      return existingDnsRecordId;
    }
    return yield* withOperationFence(ownership, dns.createRecord(dnsRecord)).pipe(
      Effect.map((record) => record.id),
      Effect.catchTags({
        ManagedEndpointDnsClientError: (createError) =>
          Effect.gen(function* () {
            let records = yield* withOperationFence(ownership, dns.listRecords(hostname));
            for (let attempt = 0; records.length === 0 && attempt < 4; attempt++) {
              yield* Effect.sleep("200 millis");
              records = yield* withOperationFence(ownership, dns.listRecords(hostname));
            }
            return records;
          }).pipe(
            Effect.flatMap((records) =>
              records.length > 0
                ? updateExistingDnsRecords(
                    ownership,
                    records,
                    preferredDnsRecordId,
                    dnsRecord,
                  ).pipe(Effect.map((dnsRecordId) => dnsRecordId!))
                : Effect.fail(createError),
            ),
          ),
      }),
    );
  });

  const findTunnel = Effect.fnUntraced(function* (
    ownership: ManagedEndpointAllocations.ManagedEndpointOperation,
    tunnelName: string,
  ) {
    const response = yield* withOperationFence(
      ownership,
      tunnels.list({ name: tunnelName, isDeleted: false }),
    );
    return Arr.findFirst(response.result, (tunnel) => tunnel.name === tunnelName);
  });

  const ensureTunnel = Effect.fnUntraced(function* (
    ownership: ManagedEndpointAllocations.ManagedEndpointOperation,
    tunnelName: string,
  ) {
    const existing = yield* findTunnel(ownership, tunnelName);
    if (Option.isSome(existing)) {
      return existing.value;
    }
    return yield* withOperationFence(
      ownership,
      tunnels.create({ name: tunnelName, configSrc: "cloudflare" }),
    ).pipe(
      Effect.catchTag("ManagedEndpointTunnelClientError", (createError) =>
        findTunnel(ownership, tunnelName).pipe(
          Effect.flatMap(
            Option.match({
              onSome: Effect.succeed,
              onNone: () => Effect.fail(createError),
            }),
          ),
        ),
      ),
    );
  });

  const deprovisionOwned = Effect.fn("relay.managed_endpoint_provider.deprovision_owned")(
    function* (
      input: ManagedEndpointDeprovisionInput & {
        readonly ownership: ManagedEndpointAllocations.ManagedEndpointOperation;
      },
    ) {
      yield* Effect.annotateCurrentSpan({
        "relay.user_id": input.userId,
        "relay.environment_id": input.environmentId,
        "relay.operation_generation": input.ownership.generation,
      });
      const allocation = yield* allocations.claimForOperation(input.ownership).pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointDeprovisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "load-allocation",
              cause,
            }),
        ),
      );
      if (allocation === null) {
        return;
      }
      yield* Effect.forEach(
        managedEndpointPreviewHostnames(allocation.hostname),
        (previewHostname) =>
          withOperationFence(input.ownership, dns.listRecords(previewHostname)).pipe(
            Effect.flatMap((records) =>
              Effect.forEach(
                records,
                (record) =>
                  withOperationFence(input.ownership, ignoreNotFound(dns.deleteRecord(record.id))),
                { discard: true },
              ),
            ),
          ),
        { discard: true },
      ).pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointDeprovisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "delete-preview-dns-record",
              cause,
            }),
        ),
      );
      const dnsRecordId = allocation.dnsRecordId;
      if (dnsRecordId !== null) {
        yield* withOperationFence(
          input.ownership,
          ignoreNotFound(dns.deleteRecord(dnsRecordId)),
        ).pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointDeprovisioningFailed({
                userId: input.userId,
                environmentId: input.environmentId,
                stage: "delete-dns-record",
                dnsRecordId,
                cause,
              }),
          ),
        );
      }
      const tunnelId = allocation.tunnelId;
      if (tunnelId !== null) {
        yield* withOperationFence(input.ownership, ignoreNotFound(tunnels.delete(tunnelId))).pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointDeprovisioningFailed({
                userId: input.userId,
                environmentId: input.environmentId,
                stage: "delete-tunnel",
                tunnelId,
                cause,
              }),
          ),
        );
      }
      yield* allocations
        .remove({
          userId: input.userId,
          environmentId: input.environmentId,
          ownership: input.ownership,
        })
        .pipe(
          Effect.mapError(
            (cause) =>
              new ManagedEndpointDeprovisioningFailed({
                userId: input.userId,
                environmentId: input.environmentId,
                stage: "remove-allocation",
                ...(allocation.tunnelId === null ? {} : { tunnelId: allocation.tunnelId }),
                ...(allocation.dnsRecordId === null ? {} : { dnsRecordId: allocation.dnsRecordId }),
                cause,
              }),
          ),
        );
    },
  );

  const provisionOwned = Effect.fn("relay.managed_endpoint_provider.provision_owned")(function* (
    input: ManagedEndpointProvisionInput & {
      readonly ownership: ManagedEndpointAllocations.ManagedEndpointOperation;
    },
  ) {
    yield* Effect.annotateCurrentSpan({
      "relay.user_id": input.userId,
      "relay.environment_id": input.environmentId,
      "relay.managed_endpoint.origin_host": input.origin.localHttpHost,
      "relay.managed_endpoint.origin_port": input.origin.localHttpPort,
      "relay.operation_generation": input.ownership.generation,
    });
    if (!isLoopbackOrigin(input.origin)) {
      return yield* new ManagedEndpointOriginNotAllowed({
        userId: input.userId,
        environmentId: input.environmentId,
        host: input.origin.localHttpHost,
        port: input.origin.localHttpPort,
      });
    }
    const cf = yield* requireCloudflareSettings(config, input);
    const environmentHash = yield* crypto
      .digest(
        "SHA-256",
        new TextEncoder().encode(
          managedEndpointDigestInput(cf.namespace, input.userId, input.environmentId),
        ),
      )
      .pipe(
        Effect.map(Hex.encode),
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "derive-environment-hash",
              cause,
            }),
        ),
      );
    const requestedHostname = managedEndpointHostname(cf.namespace, cf.baseDomain, environmentHash);
    const requestedTunnelName = managedEndpointTunnelName(cf.namespace, environmentHash);
    const previousAllocation = yield* allocations
      .get({ userId: input.userId, environmentId: input.environmentId })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "reserve-allocation",
              hostname: requestedHostname,
              tunnelName: requestedTunnelName,
              cause,
            }),
        ),
      );
    const allocation = yield* allocations
      .reserve({
        userId: input.userId,
        environmentId: input.environmentId,
        hostname: requestedHostname,
        tunnelName: requestedTunnelName,
        ownership: input.ownership,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "reserve-allocation",
              hostname: requestedHostname,
              tunnelName: requestedTunnelName,
              cause,
            }),
        ),
      );
    const { hostname, tunnelName } = allocation;
    const previewHostnames = managedEndpointPreviewHostnames(hostname);

    const tunnelResponse = yield* ensureTunnel(input.ownership, tunnelName).pipe(
      Effect.mapError(
        (cause) =>
          new ManagedEndpointProvisioningFailed({
            userId: input.userId,
            environmentId: input.environmentId,
            stage: "ensure-tunnel",
            hostname,
            tunnelName,
            cause,
          }),
      ),
    );
    if (!tunnelResponse.id || tunnelResponse.name !== tunnelName) {
      return yield* new ManagedEndpointProvisioningFailed({
        userId: input.userId,
        environmentId: input.environmentId,
        stage: "validate-tunnel-response",
        hostname,
        tunnelName,
        ...(tunnelResponse.id ? { returnedTunnelId: tunnelResponse.id } : {}),
        ...(tunnelResponse.name ? { returnedTunnelName: tunnelResponse.name } : {}),
      });
    }
    const tunnel = { id: tunnelResponse.id, name: tunnelResponse.name };
    yield* allocations
      .recordTunnel({
        userId: input.userId,
        environmentId: input.environmentId,
        tunnelId: tunnel.id,
        ownership: input.ownership,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "record-tunnel",
              hostname,
              tunnelName,
              tunnelId: tunnel.id,
              cause,
            }),
        ),
      );

    yield* withOperationFence(
      input.ownership,
      tunnels.putConfiguration(tunnel.id, {
        ingress: [
          {
            hostname,
            service: formatOriginService(input.origin),
          },
          ...previewHostnames.map((previewHostname) => ({
            hostname: previewHostname,
            service: formatOriginService(input.origin),
          })),
          { service: "http_status:404" },
        ],
      }),
    ).pipe(
      Effect.mapError(
        (cause) =>
          new ManagedEndpointProvisioningFailed({
            userId: input.userId,
            environmentId: input.environmentId,
            stage: "configure-tunnel",
            hostname,
            tunnelName,
            tunnelId: tunnel.id,
            cause,
          }),
      ),
    );

    const dnsRecord = {
      type: "CNAME",
      name: hostname,
      content: `${tunnel.id}.cfargotunnel.com`,
      ttl: 1,
      proxied: true,
    } as const;

    const dnsRecordId = yield* ensureDnsRecord(
      input.ownership,
      hostname,
      allocation.dnsRecordId,
      dnsRecord,
    ).pipe(
      Effect.mapError(
        (cause) =>
          new ManagedEndpointProvisioningFailed({
            userId: input.userId,
            environmentId: input.environmentId,
            stage: "ensure-dns-record",
            hostname,
            tunnelName,
            tunnelId: tunnel.id,
            ...(allocation.dnsRecordId === null ? {} : { dnsRecordId: allocation.dnsRecordId }),
            cause,
          }),
      ),
    );
    yield* allocations
      .recordDns({
        userId: input.userId,
        environmentId: input.environmentId,
        dnsRecordId,
        ownership: input.ownership,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "record-dns",
              hostname,
              tunnelName,
              tunnelId: tunnel.id,
              dnsRecordId,
              cause,
            }),
        ),
      );

    // Found by name on every run, so a retried or older link gains missing records
    // without duplicates and the allocation table stores nothing for them.
    yield* Effect.forEach(
      previewHostnames,
      (previewHostname) =>
        ensureDnsRecord(input.ownership, previewHostname, null, {
          ...dnsRecord,
          name: previewHostname,
        }),
      { discard: true },
    ).pipe(
      Effect.mapError(
        (cause) =>
          new ManagedEndpointProvisioningFailed({
            userId: input.userId,
            environmentId: input.environmentId,
            stage: "ensure-preview-dns-record",
            hostname,
            tunnelName,
            tunnelId: tunnel.id,
            dnsRecordId,
            cause,
          }),
      ),
    );

    const connectorToken = yield* withOperationFence(
      input.ownership,
      tunnels.getToken(tunnel.id),
    ).pipe(
      Effect.mapError(
        (cause) =>
          new ManagedEndpointProvisioningFailed({
            userId: input.userId,
            environmentId: input.environmentId,
            stage: "get-tunnel-token",
            hostname,
            tunnelName,
            tunnelId: tunnel.id,
            dnsRecordId,
            cause,
          }),
      ),
    );
    yield* allocations
      .markReady({
        userId: input.userId,
        environmentId: input.environmentId,
        ownership: input.ownership,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ManagedEndpointProvisioningFailed({
              userId: input.userId,
              environmentId: input.environmentId,
              stage: "mark-allocation-ready",
              hostname,
              tunnelName,
              tunnelId: tunnel.id,
              dnsRecordId,
              cause,
            }),
        ),
      );

    return {
      endpoint: managedEndpointForHostname(hostname),
      runtime: {
        providerKind: "cloudflare_tunnel",
        connectorToken,
        tunnelId: tunnel.id,
        tunnelName: tunnel.name,
        hostname,
        previewHostnames,
      },
      endpointDisposition:
        previousAllocation?.readyAt === null || previousAllocation === null ? "created" : "reused",
    } satisfies ManagedEndpointProvisioningResult;
  });

  return ManagedEndpointProvider.of({
    deprovision: (input) =>
      input.ownership === undefined
        ? allocations
            .withOperation(
              { userId: input.userId, environmentId: input.environmentId, kind: "deprovision" },
              (ownership) => deprovisionOwned({ ...input, ownership }),
            )
            .pipe(
              Effect.mapError((cause) =>
                cause._tag === "ManagedEndpointDeprovisioningFailed"
                  ? cause
                  : new ManagedEndpointDeprovisioningFailed({
                      userId: input.userId,
                      environmentId: input.environmentId,
                      stage: "load-allocation",
                      cause,
                    }),
              ),
            )
        : deprovisionOwned({ ...input, ownership: input.ownership }),
    provision: (input) =>
      input.ownership === undefined
        ? allocations
            .withOperation(
              { userId: input.userId, environmentId: input.environmentId, kind: "provision" },
              (ownership) => provisionOwned({ ...input, ownership }),
            )
            .pipe(
              Effect.mapError((cause) =>
                cause._tag === "ManagedEndpointAllocationPersistenceError"
                  ? new ManagedEndpointProvisioningFailed({
                      userId: input.userId,
                      environmentId: input.environmentId,
                      stage: "reserve-allocation",
                      cause,
                    })
                  : cause,
              ),
            )
        : provisionOwned({ ...input, ownership: input.ownership }),
  });
});

export const layer = Layer.effect(ManagedEndpointProvider, make);

export const layerCloudflareBindings = (
  tunnelClient: Cloudflare.Tunnel.ReadWriteTunnelClient,
  dnsClient: Cloudflare.DNS.ReadWriteDnsClient,
  alchemyRuntimeContext: Alchemy.BaseRuntimeContext,
) =>
  layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        layerTunnelClient({
          list: (request) =>
            tunnelClient.list(request).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointTunnelClientError({
                    operation: "list",
                    tunnelName: request.name,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          create: (request) =>
            tunnelClient.create(request).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointTunnelClientError({
                    operation: "create",
                    tunnelName: request.name,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          putConfiguration: (tunnelId, config) =>
            tunnelClient.putConfiguration(tunnelId, config).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointTunnelClientError({
                    operation: "put-configuration",
                    tunnelId,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          getToken: (tunnelId) =>
            tunnelClient.getToken(tunnelId).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointTunnelClientError({
                    operation: "get-token",
                    tunnelId,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          delete: (tunnelId) =>
            tunnelClient.delete(tunnelId).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointTunnelClientError({
                    operation: "delete",
                    tunnelId,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
        }),
        layerDnsClient({
          listRecords: (hostname) =>
            dnsClient.listDnsRecords({ search: hostname }).pipe(
              Effect.map((response) =>
                response.result.filter(
                  (record): record is typeof record & { readonly id: string } =>
                    typeof record.id === "string" &&
                    normalizeHostname(record.name) === normalizeHostname(hostname),
                ),
              ),
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointDnsClientError({
                    operation: "list-records",
                    hostname,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          createRecord: (request) =>
            dnsClient.createDnsRecord(request).pipe(
              Effect.map((response) => ({ id: response.id })),
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointDnsClientError({
                    operation: "create-record",
                    hostname: request.name,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          updateRecord: (dnsRecordId, request) =>
            dnsClient.updateDnsRecord(dnsRecordId, request).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointDnsClientError({
                    operation: "update-record",
                    hostname: request.name,
                    dnsRecordId,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
          deleteRecord: (dnsRecordId) =>
            dnsClient.deleteDnsRecord(dnsRecordId).pipe(
              Effect.mapError(
                (cause) =>
                  new ManagedEndpointDnsClientError({
                    operation: "delete-record",
                    dnsRecordId,
                    cause: redactProviderCause(cause),
                  }),
              ),
              Effect.provideService(Alchemy.RuntimeContext, alchemyRuntimeContext),
            ),
        }),
      ),
    ),
  );
