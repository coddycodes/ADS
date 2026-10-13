import * as NodeCrypto from "node:crypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import type * as Alchemy from "alchemy";
import type * as Cloudflare from "alchemy/Cloudflare";

import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Crypto from "effect/Crypto";
import * as Duration from "effect/Duration";
import * as Deferred from "effect/Deferred";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as PlatformError from "effect/PlatformError";
import * as Redacted from "effect/Redacted";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as TestClock from "effect/testing/TestClock";

import * as RelayConfiguration from "../Config.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";
import * as ManagedEndpointProvider from "./ManagedEndpointProvider.ts";

const encodeJson = Schema.encodeUnknownSync(Schema.fromJsonString(Schema.Unknown));

const config = RelayConfiguration.RelayConfiguration.of({
  relayIssuer: "https://relay.example.test",
  clerkSecretKey: Redacted.make("clerk-secret"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "bibcode-relay",
  cloudMintPrivateKey: Redacted.make("cloud-private-key"),
  cloudMintPublicKey: "cloud-public-key",
  managedEndpointBaseDomain: "bibcode.test",
  managedEndpointNamespace: "dev_julius",
});

interface TunnelCall {
  readonly operation: "list" | "create" | "putConfiguration" | "getToken" | "delete";
  readonly input: unknown;
}

interface DnsCall {
  readonly operation: "listRecords" | "createRecord" | "updateRecord" | "deleteRecord";
  readonly input: unknown;
}

interface AllocationCall {
  readonly operation:
    | "get"
    | "reserve"
    | "recordTunnel"
    | "recordDns"
    | "markReady"
    | "remove"
    | "renewOperation";
  readonly input: unknown;
}

function allocationKey(input: { readonly userId: string; readonly environmentId: string }) {
  return `${input.userId}:${input.environmentId}`;
}

function makeTunnelClient(calls: TunnelCall[] = []) {
  return ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
    list: (request) =>
      Effect.sync(() => {
        calls.push({ operation: "list", input: request });
        return { result: [] };
      }),
    create: (request) =>
      Effect.sync(() => {
        calls.push({ operation: "create", input: request });
        return { id: "tunnel-id", name: request.name };
      }),
    putConfiguration: (tunnelId, tunnelConfig) =>
      Effect.sync(() => {
        calls.push({ operation: "putConfiguration", input: { tunnelId, tunnelConfig } });
      }),
    getToken: (tunnelId) =>
      Effect.sync(() => {
        calls.push({ operation: "getToken", input: tunnelId });
        return "connector-token";
      }),
    delete: (tunnelId) =>
      Effect.sync(() => {
        calls.push({ operation: "delete", input: tunnelId });
      }),
  });
}

function makePersistentTunnelClient(calls: TunnelCall[] = []) {
  let tunnel: { readonly id: string; readonly name: string } | null = null;
  return ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
    list: (request) =>
      Effect.sync(() => {
        calls.push({ operation: "list", input: request });
        return { result: tunnel === null ? [] : [tunnel] };
      }),
    create: (request) =>
      Effect.sync(() => {
        calls.push({ operation: "create", input: request });
        tunnel = { id: "tunnel-id", name: request.name };
        return tunnel;
      }),
    putConfiguration: (tunnelId, tunnelConfig) =>
      Effect.sync(() => {
        calls.push({ operation: "putConfiguration", input: { tunnelId, tunnelConfig } });
      }),
    getToken: (tunnelId) =>
      Effect.sync(() => {
        calls.push({ operation: "getToken", input: tunnelId });
        return "connector-token";
      }),
    delete: (tunnelId) =>
      Effect.sync(() => {
        calls.push({ operation: "delete", input: tunnelId });
        tunnel = null;
      }),
  });
}

const PREVIEW_HOSTNAME = /-p[1-4]\./u;

function makeDnsClient(
  calls: DnsCall[] = [],
  records: ReadonlyArray<{ readonly id: string }> = [],
  previewCalls: DnsCall[] = [],
) {
  let currentRecords = [...records];
  const previewRecords = new Map<string, { readonly id: string }>();
  const previewName = (dnsRecordId: string) =>
    dnsRecordId.startsWith("preview:") ? dnsRecordId.slice("preview:".length) : null;
  return ManagedEndpointProvider.ManagedEndpointDnsClient.of({
    listRecords: (hostname) =>
      Effect.sync(() => {
        if (PREVIEW_HOSTNAME.test(hostname)) {
          previewCalls.push({ operation: "listRecords", input: hostname });
          const record = previewRecords.get(hostname);
          return record === undefined ? [] : [record];
        }
        calls.push({ operation: "listRecords", input: hostname });
        return currentRecords;
      }),
    createRecord: (request) =>
      Effect.sync(() => {
        if (PREVIEW_HOSTNAME.test(request.name)) {
          previewCalls.push({ operation: "createRecord", input: request });
          const record = { id: `preview:${request.name}` };
          previewRecords.set(request.name, record);
          return record;
        }
        calls.push({ operation: "createRecord", input: request });
        const record = { id: "created-record-id" };
        currentRecords = [record];
        return record;
      }),
    updateRecord: (dnsRecordId, request) =>
      Effect.gen(function* () {
        const preview = previewName(dnsRecordId);
        if (preview !== null) {
          previewCalls.push({ operation: "updateRecord", input: { dnsRecordId, request } });
          return;
        }
        calls.push({ operation: "updateRecord", input: { dnsRecordId, request } });
        if (!currentRecords.some((record) => record.id === dnsRecordId)) {
          return yield* new ManagedEndpointProvider.ManagedEndpointDnsClientError({
            operation: "update-record",
            hostname: request.name,
            dnsRecordId,
            cause: { _tag: "NotFound", dnsRecordId },
          });
        }
      }),
    deleteRecord: (dnsRecordId) =>
      Effect.sync(() => {
        const preview = previewName(dnsRecordId);
        if (preview !== null) {
          previewCalls.push({ operation: "deleteRecord", input: dnsRecordId });
          previewRecords.delete(preview);
          return;
        }
        calls.push({ operation: "deleteRecord", input: dnsRecordId });
        currentRecords = currentRecords.filter((record) => record.id !== dnsRecordId);
      }),
  });
}

function makeAllocations(calls: AllocationCall[] = []) {
  const allocations = new Map<string, ManagedEndpointAllocations.ManagedEndpointAllocation>();
  return ManagedEndpointAllocations.ManagedEndpointAllocations.of({
    withOperation: (input, use) =>
      use({ ...input, generation: 1, ownerToken: "test-operation-owner" }),
    acquireOperation: (input) =>
      Effect.succeed({ ...input, generation: 1, ownerToken: "test-operation-owner" }),
    releaseOperation: () => Effect.void,
    renewOperation: (operation) =>
      Effect.sync(() => {
        calls.push({ operation: "renewOperation", input: operation });
      }),
    claimForOperation: (operation) =>
      Effect.sync(() => {
        calls.push({
          operation: "get",
          input: { userId: operation.userId, environmentId: operation.environmentId },
        });
        return allocations.get(allocationKey(operation)) ?? null;
      }),
    get: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "get", input });
        return allocations.get(allocationKey(input)) ?? null;
      }),
    reserve: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "reserve", input });
        const allocation = allocations.get(allocationKey(input)) ?? {
          ...input,
          tunnelId: null,
          dnsRecordId: null,
          readyAt: null,
        };
        allocations.set(allocationKey(input), allocation);
        return allocation;
      }),
    recordTunnel: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "recordTunnel", input });
        const allocation = allocations.get(allocationKey(input));
        if (allocation !== undefined) {
          allocations.set(allocationKey(input), { ...allocation, tunnelId: input.tunnelId });
        }
      }),
    recordDns: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "recordDns", input });
        const allocation = allocations.get(allocationKey(input));
        if (allocation !== undefined) {
          allocations.set(allocationKey(input), { ...allocation, dnsRecordId: input.dnsRecordId });
        }
      }),
    markReady: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "markReady", input });
        const allocation = allocations.get(allocationKey(input));
        if (allocation !== undefined) {
          allocations.set(allocationKey(input), {
            ...allocation,
            readyAt: "2026-06-02T00:00:00.000Z",
          });
        }
      }),
    remove: (input) =>
      Effect.sync(() => {
        calls.push({ operation: "remove", input });
        allocations.delete(allocationKey(input));
      }),
  });
}

function providerLayer(
  tunnelClient = makeTunnelClient(),
  dnsClient = makeDnsClient(),
  allocations = makeAllocations(),
  configuration: RelayConfiguration.RelayConfiguration["Service"] = config,
  crypto?: Crypto.Crypto,
) {
  return ManagedEndpointProvider.layer.pipe(
    Layer.provideMerge(
      crypto === undefined ? NodeServices.layer : Layer.succeed(Crypto.Crypto, crypto),
    ),
    Layer.provide(RelayConfiguration.layer(configuration)),
    Layer.provide(ManagedEndpointProvider.layerTunnelClient(tunnelClient)),
    Layer.provide(ManagedEndpointProvider.layerDnsClient(dnsClient)),
    Layer.provide(
      Layer.succeed(ManagedEndpointAllocations.ManagedEndpointAllocations, allocations),
    ),
  );
}

function cloudflareProviderLayer(
  tunnelClient: Cloudflare.Tunnel.ReadWriteTunnelClient,
  dnsClient: Cloudflare.DNS.ReadWriteDnsClient,
  allocations = makeAllocations(),
) {
  return ManagedEndpointProvider.layerCloudflareBindings(
    tunnelClient,
    dnsClient,
    {} as Alchemy.BaseRuntimeContext,
  ).pipe(
    Layer.provideMerge(NodeServices.layer),
    Layer.provide(RelayConfiguration.layer(config)),
    Layer.provide(
      Layer.succeed(ManagedEndpointAllocations.ManagedEndpointAllocations, allocations),
    ),
  );
}

function expectedManagedHostname(environmentId: string, userId = "user_ABC"): string {
  const hash = NodeCrypto.createHash("sha256")
    .update(`dev_julius:${userId}:${environmentId}`)
    .digest("hex")
    .slice(0, 16);
  return `dev-julius-${hash}.bibcode.test`;
}

function expectedPreviewHostnames(environmentId: string, userId = "user_ABC"): string[] {
  const [label, ...zone] = expectedManagedHostname(environmentId, userId).split(".");
  return [1, 2, 3, 4].map((slot) => [`${label}-p${slot}`, ...zone].join("."));
}

function expectedManagedTunnelName(environmentId: string, userId = "user_ABC"): string {
  const hash = NodeCrypto.createHash("sha256")
    .update(`dev_julius:${userId}:${environmentId}`)
    .digest("hex")
    .slice(0, 16);
  return `bibcoderelay-managedendpoint-dev-julius-${hash}`;
}

describe("ManagedEndpointProvider", () => {
  it("keeps provider timeout strictly inside the production operation lease", () => {
    const leaseMillis = (ManagedEndpointAllocations as unknown as Record<string, unknown>)
      .MANAGED_ENDPOINT_OPERATION_LEASE_MILLIS;
    const timeoutMillis = (ManagedEndpointProvider as unknown as Record<string, unknown>)
      .MANAGED_ENDPOINT_PROVIDER_CALL_TIMEOUT_MILLIS;

    expect(leaseMillis).toBeTypeOf("number");
    expect(timeoutMillis).toBeTypeOf("number");
    expect(timeoutMillis).toBeLessThan(leaseMillis as number);
  });

  it("formats provider errors without serializing causes or connector tokens", () => {
    const cause = new Error("connector-token-secret");
    const errors = [
      new ManagedEndpointProvider.ManagedEndpointProvisioningNotConfigured({
        userId: "user-1",
        environmentId: "env-1",
        missingSettings: ["managedEndpointBaseDomain", "managedEndpointNamespace"],
      }),
      new ManagedEndpointProvider.ManagedEndpointProvisioningFailed({
        stage: "ensure-tunnel",
        userId: "user-1",
        environmentId: "env-1",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointDeprovisioningFailed({
        stage: "delete-tunnel",
        userId: "user-1",
        environmentId: "env-1",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointOriginNotAllowed({
        userId: "user-1",
        environmentId: "env-1",
        host: "example.test",
        port: 3773,
      }),
      new ManagedEndpointProvider.ManagedEndpointProviderCallTimedOut({
        timeoutMillis: 240_000,
      }),
      new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
        operation: "delete",
        tunnelId: "tunnel-1",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
        operation: "list",
        tunnelName: "tunnel-name",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
        operation: "list",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointDnsClientError({
        operation: "delete-record",
        dnsRecordId: "dns-1",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointDnsClientError({
        operation: "list-records",
        hostname: "env.example.test",
        cause,
      }),
      new ManagedEndpointProvider.ManagedEndpointDnsClientError({
        operation: "list-records",
        cause,
      }),
    ];
    expect(errors.map((error) => error.message)).toEqual([
      "Managed endpoint provisioning is not configured for user 'user-1', environment 'env-1': missing managedEndpointBaseDomain, managedEndpointNamespace",
      "Managed endpoint provisioning failed during 'ensure-tunnel' for user 'user-1', environment 'env-1'",
      "Managed endpoint deprovisioning failed during 'delete-tunnel' for user 'user-1', environment 'env-1'",
      "Managed endpoint origin 'example.test:3773' is not allowed for user 'user-1', environment 'env-1'",
      "Managed endpoint provider call exceeded 240000ms",
      "Managed endpoint tunnel provider 'delete' request failed for 'tunnel-1'",
      "Managed endpoint tunnel provider 'list' request failed for 'tunnel-name'",
      "Managed endpoint tunnel provider 'list' request failed",
      "Managed endpoint DNS provider 'delete-record' request failed for 'dns-1'",
      "Managed endpoint DNS provider 'list-records' request failed for 'env.example.test'",
      "Managed endpoint DNS provider 'list-records' request failed",
    ]);
    for (const error of errors) {
      expect(JSON.stringify(error)).not.toContain("connector-token-secret");
    }
  });

  it.effect("reports every missing managed endpoint setting", () => {
    const missingConfig = {
      ...config,
      managedEndpointBaseDomain: undefined,
      managedEndpointNamespace: undefined,
    };
    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.provision({
          userId: "user-1",
          environmentId: "env-1",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );
      expect(error).toMatchObject({
        _tag: "ManagedEndpointProvisioningNotConfigured",
        missingSettings: ["managedEndpointBaseDomain", "managedEndpointNamespace"],
      });
    }).pipe(Effect.provide(providerLayer(undefined, undefined, undefined, missingConfig)));
  });

  it.effect("maps every provisioning checkpoint failure to its stable stage", () =>
    Effect.gen(function* () {
      const nodeCrypto = yield* Crypto.Crypto;
      const allocationFailure = (operation: string) =>
        new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
          operation: operation as never,
          stage: "database-request",
          userId: "user_ABC",
          environmentId: "env-stage",
          cause: new Error(`${operation} failed`),
        });
      const tunnelFailure = (operation: string) =>
        new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
          operation: operation as never,
          cause: new Error(`${operation} failed`),
        });
      const run = (
        tunnelClient: ManagedEndpointProvider.ManagedEndpointTunnelClient["Service"],
        dnsClient: ManagedEndpointProvider.ManagedEndpointDnsClient["Service"],
        allocations: ManagedEndpointAllocations.ManagedEndpointAllocations["Service"],
        crypto?: Crypto.Crypto,
      ) =>
        Effect.gen(function* () {
          const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
          return yield* Effect.flip(
            provider.provision({
              userId: "user_ABC",
              environmentId: "env-stage",
              origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
            }),
          );
        }).pipe(
          Effect.provide(providerLayer(tunnelClient, dnsClient, allocations, config, crypto)),
        );

      const baseTunnel = makeTunnelClient();
      const baseDns = makeDnsClient();
      const derive = yield* run(baseTunnel, baseDns, makeAllocations(), {
        ...nodeCrypto,
        digest: () =>
          Effect.fail(
            PlatformError.badArgument({
              module: "Crypto",
              method: "digest",
              description: "digest failed",
            }),
          ),
      });
      const reserveBase = makeAllocations();
      const reserve = yield* run(baseTunnel, baseDns, {
        ...reserveBase,
        reserve: () => Effect.fail(allocationFailure("reserve")),
      });
      const ensureTunnel = yield* run(
        {
          ...baseTunnel,
          list: () => Effect.fail(tunnelFailure("list")),
        },
        baseDns,
        makeAllocations(),
      );
      const recordTunnelBase = makeAllocations();
      const recordTunnel = yield* run(baseTunnel, baseDns, {
        ...recordTunnelBase,
        recordTunnel: () => Effect.fail(allocationFailure("record-tunnel")),
      });
      const configureTunnel = yield* run(
        {
          ...baseTunnel,
          putConfiguration: () => Effect.fail(tunnelFailure("put-configuration")),
        },
        baseDns,
        makeAllocations(),
      );
      const recordDnsBase = makeAllocations();
      const recordDns = yield* run(baseTunnel, baseDns, {
        ...recordDnsBase,
        recordDns: () => Effect.fail(allocationFailure("record-dns")),
      });
      const getToken = yield* run(
        {
          ...baseTunnel,
          getToken: () => Effect.fail(tunnelFailure("get-token")),
        },
        baseDns,
        makeAllocations(),
      );
      const markReadyBase = makeAllocations();
      const markReady = yield* run(baseTunnel, baseDns, {
        ...markReadyBase,
        markReady: () => Effect.fail(allocationFailure("mark-ready")),
      });

      const errors = [
        derive,
        reserve,
        ensureTunnel,
        recordTunnel,
        configureTunnel,
        recordDns,
        getToken,
        markReady,
      ];
      expect(errors.every((error) => error._tag === "ManagedEndpointProvisioningFailed")).toBe(
        true,
      );
      const provisioningErrors = errors.filter(
        (error): error is ManagedEndpointProvider.ManagedEndpointProvisioningFailed =>
          error._tag === "ManagedEndpointProvisioningFailed",
      );
      expect(provisioningErrors.map((error) => error.stage)).toEqual([
        "derive-environment-hash",
        "reserve-allocation",
        "ensure-tunnel",
        "record-tunnel",
        "configure-tunnel",
        "record-dns",
        "get-tunnel-token",
        "mark-allocation-ready",
      ]);
    }).pipe(Effect.provide(NodeServices.layer)),
  );

  it.effect("maps deprovisioning persistence and DNS failures without removing state early", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env-stage",
      hostname: "env.bibcode.test",
      tunnelId: "tunnel-id",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-id",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    const allocationError =
      new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
        operation: "get",
        stage: "database-request",
        userId: allocation.userId,
        environmentId: allocation.environmentId,
        cause: new Error("database failed"),
      });
    const dnsError = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "delete-record",
      dnsRecordId: "dns-id",
      cause: "not-found-shape-is-primitive",
    });
    const removeError = new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
      operation: "remove",
      stage: "database-request",
      userId: allocation.userId,
      environmentId: allocation.environmentId,
      cause: new Error("remove failed"),
    });
    const run = (
      allocations: ManagedEndpointAllocations.ManagedEndpointAllocations["Service"],
      dns = makeDnsClient(),
    ) =>
      Effect.gen(function* () {
        const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
        return yield* Effect.flip(
          provider.deprovision({
            userId: allocation.userId,
            environmentId: allocation.environmentId,
          }),
        );
      }).pipe(Effect.provide(providerLayer(makeTunnelClient(), dns, allocations)));
    const base = makeAllocations();

    return Effect.gen(function* () {
      const load = yield* run({ ...base, claimForOperation: () => Effect.fail(allocationError) });
      const dns = yield* run(
        { ...base, claimForOperation: () => Effect.succeed(allocation) },
        { ...makeDnsClient(), deleteRecord: () => Effect.fail(dnsError) },
      );
      const remove = yield* run({
        ...base,
        claimForOperation: () => Effect.succeed(allocation),
        remove: () => Effect.fail(removeError),
      });

      expect([load.stage, dns.stage, remove.stage]).toEqual([
        "load-allocation",
        "delete-dns-record",
        "remove-allocation",
      ]);
      expect(remove).toMatchObject({ tunnelId: "tunnel-id", dnsRecordId: "dns-id" });
    });
  });

  it.effect("adapts Cloudflare tunnel and DNS bindings without leaking provider details", () => {
    const tunnelCalls: string[] = [];
    const dnsCalls: string[] = [];
    let tunnel: { readonly id: string; readonly name: string } | null = null;
    const dnsRecords = new Map<string, { readonly id: string; readonly name: string }>();
    const tunnelClient = {
      list: (_request: { readonly name: string }) =>
        Effect.sync(() => {
          tunnelCalls.push("list");
          return { result: tunnel === null ? [] : [tunnel] };
        }),
      create: (request: { readonly name: string }) =>
        Effect.sync(() => {
          tunnelCalls.push("create");
          tunnel = { id: "tunnel-id", name: request.name };
          return tunnel;
        }),
      putConfiguration: () =>
        Effect.sync(() => {
          tunnelCalls.push("put-configuration");
        }),
      getToken: () =>
        Effect.sync(() => {
          tunnelCalls.push("get-token");
          return "connector-token";
        }),
      delete: () =>
        Effect.sync(() => {
          tunnelCalls.push("delete");
          tunnel = null;
        }),
    } as unknown as Cloudflare.Tunnel.ReadWriteTunnelClient;
    const dnsClient = {
      listDnsRecords: () =>
        Effect.sync(() => {
          dnsCalls.push("list-records");
          return { result: [...dnsRecords.values()] };
        }),
      createDnsRecord: (request: { readonly name: string }) =>
        Effect.sync(() => {
          dnsCalls.push("create-record");
          const record = { id: `dns-${request.name}`, name: request.name };
          dnsRecords.set(record.id, record);
          return { id: record.id };
        }),
      updateDnsRecord: () =>
        Effect.sync(() => {
          dnsCalls.push("update-record");
        }),
      deleteDnsRecord: (id: string) =>
        Effect.sync(() => {
          dnsCalls.push("delete-record");
          dnsRecords.delete(id);
        }),
    } as unknown as Cloudflare.DNS.ReadWriteDnsClient;
    const allocations = makeAllocations();

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const provisioned = yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env-bindings",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      expect(provisioned.runtime.connectorToken).toBe("connector-token");
      yield* provider.deprovision({ userId: "user_ABC", environmentId: "env-bindings" });
      expect(tunnelCalls).toEqual(["list", "create", "put-configuration", "get-token", "delete"]);
      expect(dnsCalls).toEqual([
        "list-records",
        "create-record",
        ...Array.from({ length: 4 }, () => ["list-records", "create-record"]).flat(),
        ...Array.from({ length: 4 }, () => ["list-records", "delete-record"]).flat(),
        "delete-record",
      ]);
    }).pipe(Effect.provide(cloudflareProviderLayer(tunnelClient, dnsClient, allocations)));
  });

  it.effect("filters and normalizes Cloudflare DNS records before updating the winner", () => {
    const updated: string[] = [];
    const deleted: string[] = [];
    const tunnelClient = {
      list: (request: { readonly name: string }) =>
        Effect.succeed({ result: [{ id: "tunnel-id", name: request.name }] }),
      create: () => Effect.die("create must not run"),
      putConfiguration: () => Effect.void,
      getToken: () => Effect.succeed("connector-token"),
      delete: () => Effect.void,
    } as unknown as Cloudflare.Tunnel.ReadWriteTunnelClient;
    const dnsClient = {
      listDnsRecords: (request: { readonly search: string }) =>
        Effect.succeed({
          result: PREVIEW_HOSTNAME.test(request.search)
            ? []
            : [
                { id: 42, name: request.search },
                { id: "wrong-host", name: "other.example.test" },
                { id: "matching", name: ` ${request.search.toUpperCase()}. ` },
                { id: "duplicate", name: request.search },
              ],
        }),
      createDnsRecord: (request: { readonly name: string }) =>
        PREVIEW_HOSTNAME.test(request.name)
          ? Effect.succeed({ id: "preview-id" })
          : Effect.die("create must not run"),
      updateDnsRecord: (id: string) =>
        Effect.sync(() => {
          updated.push(id);
        }),
      deleteDnsRecord: (id: string) =>
        Effect.sync(() => {
          deleted.push(id);
        }),
    } as unknown as Cloudflare.DNS.ReadWriteDnsClient;

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env-existing-dns",
        origin: { localHttpHost: "localhost", localHttpPort: 3773 },
      });
      expect(updated).toEqual(["matching"]);
      expect(deleted).toEqual(["duplicate"]);
    }).pipe(Effect.provide(cloudflareProviderLayer(tunnelClient, dnsClient)));
  });

  it.effect("recovers a Cloudflare DNS create collision through the binding adapter", () => {
    let listCalls = 0;
    const tunnelClient = {
      list: (request: { readonly name: string }) =>
        Effect.succeed({ result: [{ id: "tunnel-id", name: request.name }] }),
      create: () => Effect.die("create must not run"),
      putConfiguration: () => Effect.void,
      getToken: () => Effect.succeed("connector-token"),
      delete: () => Effect.void,
    } as unknown as Cloudflare.Tunnel.ReadWriteTunnelClient;
    const dnsClient = {
      listDnsRecords: (request: { readonly search: string }) =>
        Effect.sync(() => ({
          result: listCalls++ === 0 ? [] : [{ id: "dns-winner", name: request.search }],
        })),
      createDnsRecord: () =>
        Effect.fail(
          PlatformError.badArgument({
            module: "CloudflareDns",
            method: "createDnsRecord",
            description: "provider collision",
          }),
        ),
      updateDnsRecord: () => Effect.void,
      deleteDnsRecord: () => Effect.void,
    } as unknown as Cloudflare.DNS.ReadWriteDnsClient;

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env-dns-collision",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      expect(result.runtime.connectorToken).toBe("connector-token");
      // main lookup + retry lookup, then one lookup per preview hostname that finds the winner
      expect(listCalls).toBe(2 + 4);
    }).pipe(Effect.provide(cloudflareProviderLayer(tunnelClient, dnsClient)));
  });

  it.effect("reconciles an ambiguous tunnel-name collision from the current provider state", () => {
    let listCalls = 0;
    const tunnelName = expectedManagedTunnelName("env-tunnel-collision");
    const collision = new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
      operation: "create",
      tunnelName,
      cause: { _tag: "DuplicateTunnelName" },
    });
    const tunnels = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
      list: () =>
        Effect.sync(() => ({
          result: listCalls++ === 0 ? [] : [{ id: "tunnel-after-collision", name: tunnelName }],
        })),
      create: () => Effect.fail(collision),
      putConfiguration: () => Effect.void,
      getToken: () => Effect.succeed("connector-token"),
      delete: () => Effect.void,
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env-tunnel-collision",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      expect(result.runtime.tunnelId).toBe("tunnel-after-collision");
      expect(listCalls).toBe(2);
    }).pipe(Effect.provide(providerLayer(tunnels)));
  });

  it.effect("uses caller-owned operations and maps operation acquisition failures", () => {
    const ownership: ManagedEndpointAllocations.ManagedEndpointOperation = {
      userId: "user_ABC",
      environmentId: "env-owned",
      kind: "provision",
      generation: 9,
      ownerToken: "owner-9",
    };
    const allocationError =
      new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
        operation: "acquire-operation",
        stage: "database-request",
        userId: ownership.userId,
        environmentId: ownership.environmentId,
        cause: new Error("database unavailable"),
      });
    const base = makeAllocations();
    const unavailable = {
      ...base,
      withOperation: () => Effect.fail(allocationError),
    } as ManagedEndpointAllocations.ManagedEndpointAllocations["Service"];

    const ownedRun = Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const provisioned = yield* provider.provision({
        userId: ownership.userId,
        environmentId: ownership.environmentId,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        ownership,
      });
      yield* provider.deprovision({
        userId: ownership.userId,
        environmentId: ownership.environmentId,
        ownership: { ...ownership, kind: "deprovision" },
      });
      return provisioned;
    }).pipe(Effect.provide(providerLayer(makePersistentTunnelClient(), makeDnsClient(), base)));
    const unavailableRun = Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      return yield* Effect.all([
        Effect.flip(
          provider.provision({
            userId: ownership.userId,
            environmentId: ownership.environmentId,
            origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
          }),
        ),
        Effect.flip(
          provider.deprovision({
            userId: ownership.userId,
            environmentId: ownership.environmentId,
          }),
        ),
      ]);
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(), makeDnsClient(), unavailable)));

    return Effect.gen(function* () {
      const provisioned = yield* ownedRun;
      const acquisitionErrors = yield* unavailableRun;
      expect(provisioned.runtime.connectorToken).toBe("connector-token");
      expect(
        acquisitionErrors.map((error) => ("stage" in error ? error.stage : "missing")),
      ).toEqual(["reserve-allocation", "load-allocation"]);
    });
  });

  it.effect("maps every Cloudflare binding failure to its provider operation", () => {
    type FailureOperation =
      | "list"
      | "create"
      | "put-configuration"
      | "get-token"
      | "delete-tunnel"
      | "list-records"
      | "update-record"
      | "delete-record";
    const makeClients = (failure: FailureOperation) => {
      const fail = () =>
        Effect.fail(
          PlatformError.badArgument({
            module: "Cloudflare",
            method: failure,
            description: `secret-${failure}`,
          }),
        );
      const tunnelClient = {
        list: (request: { readonly name: string }) =>
          failure === "list"
            ? fail()
            : Effect.succeed({
                result: failure === "create" ? [] : [{ id: "tunnel-id", name: request.name }],
              }),
        create: (request: { readonly name: string }) =>
          failure === "create" ? fail() : Effect.succeed({ id: "tunnel-id", name: request.name }),
        putConfiguration: () => (failure === "put-configuration" ? fail() : Effect.void),
        getToken: () => (failure === "get-token" ? fail() : Effect.succeed("connector-token")),
        delete: () => (failure === "delete-tunnel" ? fail() : Effect.void),
      } as unknown as Cloudflare.Tunnel.ReadWriteTunnelClient;
      const dnsClient = {
        listDnsRecords: () =>
          failure === "list-records" ? fail() : Effect.succeed({ result: [] }),
        createDnsRecord: () => Effect.succeed({ id: "dns-id" }),
        updateDnsRecord: () => (failure === "update-record" ? fail() : Effect.void),
        deleteDnsRecord: () => (failure === "delete-record" ? fail() : Effect.void),
      } as unknown as Cloudflare.DNS.ReadWriteDnsClient;
      return { tunnelClient, dnsClient };
    };
    const existingAllocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env-bindings-failure",
      hostname: "env.bibcode.test",
      tunnelId: "tunnel-id",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-id",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    const operationLayer = (failure: FailureOperation) => {
      const clients = makeClients(failure);
      const base = makeAllocations();
      const allocations =
        failure === "update-record"
          ? {
              ...base,
              reserve: () => Effect.succeed({ ...existingAllocation, readyAt: null }),
            }
          : failure === "delete-tunnel" || failure === "delete-record"
            ? { ...base, claimForOperation: () => Effect.succeed(existingAllocation) }
            : base;
      return cloudflareProviderLayer(clients.tunnelClient, clients.dnsClient, allocations);
    };
    const runProvision = (failure: FailureOperation) =>
      Effect.gen(function* () {
        const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
        const result = yield* Effect.result(
          provider.provision({
            userId: existingAllocation.userId,
            environmentId: existingAllocation.environmentId,
            origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
          }),
        );
        expect(Result.isFailure(result)).toBe(true);
        return Result.isFailure(result)
          ? result.failure
          : yield* Effect.die("provision unexpectedly succeeded");
      }).pipe(Effect.provide(operationLayer(failure)));
    const runDeprovision = (failure: "delete-tunnel" | "delete-record") =>
      Effect.gen(function* () {
        const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
        const result = yield* Effect.result(
          provider.deprovision({
            userId: existingAllocation.userId,
            environmentId: existingAllocation.environmentId,
          }),
        );
        expect(Result.isFailure(result)).toBe(true);
        return Result.isFailure(result)
          ? result.failure
          : yield* Effect.die("deprovision unexpectedly succeeded");
      }).pipe(Effect.provide(operationLayer(failure)));

    return Effect.gen(function* () {
      const errors = yield* Effect.all([
        runProvision("list"),
        runProvision("create"),
        runProvision("put-configuration"),
        runProvision("get-token"),
        runDeprovision("delete-tunnel"),
        runProvision("list-records"),
        runProvision("update-record"),
        runDeprovision("delete-record"),
      ]);
      expect(errors.map((error) => ("stage" in error ? error.stage : "unexpected"))).toEqual([
        "ensure-tunnel",
        "ensure-tunnel",
        "configure-tunnel",
        "get-tunnel-token",
        "delete-tunnel",
        "ensure-dns-record",
        "ensure-dns-record",
        "delete-dns-record",
      ]);
      expect(errors.every((error) => !error.message.includes("secret-"))).toBe(true);
      expect(errors.every((error) => !encodeJson(error.toJSON()).includes("secret-"))).toBe(true);
    });
  });

  it.effect("preserves idempotent not-found cleanup through redacted Cloudflare bindings", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env-cloudflare-not-found",
      hostname: "env.bibcode.test",
      tunnelId: "tunnel-id",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-id",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    let removed = false;
    const baseAllocations = makeAllocations();
    const allocations = {
      ...baseAllocations,
      claimForOperation: () => Effect.succeed(allocation),
      remove: () => Effect.sync(() => void (removed = true)),
    };
    const notFound = { _tag: "NotFound" } as const;
    const tunnelClient = {
      list: () => Effect.succeed({ result: [] }),
      create: () => Effect.die("create must not run"),
      putConfiguration: () => Effect.die("configuration must not run"),
      getToken: () => Effect.die("token must not run"),
      delete: () => Effect.fail(notFound),
    } as unknown as Cloudflare.Tunnel.ReadWriteTunnelClient;
    const dnsClient = {
      listDnsRecords: () => Effect.succeed({ result: [] }),
      createDnsRecord: () => Effect.die("create must not run"),
      updateDnsRecord: () => Effect.die("update must not run"),
      deleteDnsRecord: () => Effect.fail(notFound),
    } as unknown as Cloudflare.DNS.ReadWriteDnsClient;

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.deprovision({
        userId: allocation.userId,
        environmentId: allocation.environmentId,
      });
      expect(removed).toBe(true);
    }).pipe(Effect.provide(cloudflareProviderLayer(tunnelClient, dnsClient, allocations)));
  });

  it.effect("retries delayed DNS collision visibility with virtual time", () => {
    let resolveRetryStarted: (() => void) | undefined;
    const retryStarted = new Promise<void>((resolve) => {
      resolveRetryStarted = resolve;
    });
    let createFailed = false;
    let retryLists = 0;
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "create-record",
      hostname: "env.bibcode.test",
      cause: "ambiguous create",
    });
    // Only the environment record collides; preview records are created normally.
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: (hostname) =>
        Effect.sync(() => {
          if (!createFailed || PREVIEW_HOSTNAME.test(hostname)) return [];
          retryLists++;
          if (retryLists === 1) {
            resolveRetryStarted?.();
            return [];
          }
          return [{ id: "dns-after-delay" }];
        }),
      createRecord: (request) =>
        PREVIEW_HOSTNAME.test(request.name)
          ? Effect.succeed({ id: `preview:${request.name}` })
          : Effect.gen(function* () {
              createFailed = true;
              return yield* failure;
            }),
      updateRecord: () => Effect.void,
      deleteRecord: () => Effect.void,
    });
    const layer = providerLayer(makeTunnelClient(), dnsClient);

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const fiber = yield* provider
        .provision({
          userId: "user_ABC",
          environmentId: "env-delayed-dns",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        })
        .pipe(Effect.forkScoped);
      yield* Effect.promise(() => retryStarted);
      yield* Effect.yieldNow;
      yield* TestClock.adjust(Duration.millis(200));
      expect((yield* Fiber.join(fiber)).runtime.connectorToken).toBe("connector-token");
      expect(retryLists).toBe(2);
    }).pipe(Effect.provide(Layer.merge(TestClock.layer(), layer)));
  });

  it.effect("bounds DNS collision retries and returns the original create error", () => {
    let resolveRetryStarted: (() => void) | undefined;
    const retryStarted = new Promise<void>((resolve) => {
      resolveRetryStarted = resolve;
    });
    let createFailed = false;
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "create-record",
      hostname: "env.bibcode.test",
      cause: "ambiguous create",
    });
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: () =>
        Effect.sync(() => {
          if (createFailed) resolveRetryStarted?.();
          return [];
        }),
      createRecord: () =>
        Effect.gen(function* () {
          createFailed = true;
          return yield* failure;
        }),
      updateRecord: () => Effect.void,
      deleteRecord: () => Effect.void,
    });
    const layer = providerLayer(makeTunnelClient(), dnsClient);

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const fiber = yield* provider
        .provision({
          userId: "user_ABC",
          environmentId: "env-missing-dns",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        })
        .pipe(Effect.flip, Effect.forkScoped);
      yield* Effect.promise(() => retryStarted);
      yield* Effect.yieldNow;
      yield* TestClock.adjust(Duration.seconds(1));
      const error = yield* Fiber.join(fiber);
      expect(error).toMatchObject({ stage: "ensure-dns-record", cause: failure });
    }).pipe(Effect.provide(Layer.merge(TestClock.layer(), layer)));
  });

  it.effect("recognizes nested and status-shaped not-found cleanup responses", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env-cleanup",
      hostname: "env.bibcode.test",
      tunnelId: "tunnel-id",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-id",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    const base = makeAllocations();
    const allocations = { ...base, claimForOperation: () => Effect.succeed(allocation) };
    const dnsClient = {
      ...makeDnsClient(),
      deleteRecord: () =>
        Effect.fail(
          new ManagedEndpointProvider.ManagedEndpointDnsClientError({
            operation: "delete-record",
            dnsRecordId: "dns-id",
            cause: { cause: { status: 404 } },
          }),
        ),
    };
    const tunnelClient = {
      ...makeTunnelClient(),
      delete: () =>
        Effect.fail(
          new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
            operation: "delete",
            tunnelId: "tunnel-id",
            cause: { status: 404 },
          }),
        ),
    };

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.deprovision({
        userId: allocation.userId,
        environmentId: allocation.environmentId,
      });
    }).pipe(Effect.provide(providerLayer(tunnelClient, dnsClient, allocations)));
  });

  it.effect("omits absent checkpoint ids from remove-allocation failures", () => {
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env-cleanup",
      hostname: "env.bibcode.test",
      tunnelId: null,
      tunnelName: "tunnel-name",
      dnsRecordId: null,
      readyAt: null,
    };
    const base = makeAllocations();
    const cause = new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
      operation: "remove",
      stage: "database-request",
      userId: allocation.userId,
      environmentId: allocation.environmentId,
    });
    const allocations = {
      ...base,
      claimForOperation: () => Effect.succeed(allocation),
      remove: () => Effect.fail(cause),
    };

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.deprovision({
          userId: allocation.userId,
          environmentId: allocation.environmentId,
        }),
      );
      expect(error.stage).toBe("remove-allocation");
      expect(error.tunnelId).toBeUndefined();
      expect(error.dnsRecordId).toBeUndefined();
    }).pipe(Effect.provide(providerLayer(undefined, undefined, allocations)));
  });

  it.effect("omits absent tunnel response fields from validation failures", () => {
    const tunnelClient = {
      ...makeTunnelClient(),
      list: () => Effect.succeed({ result: [] }),
      create: () => Effect.succeed({ id: null, name: null }),
    };
    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.provision({
          userId: "user_ABC",
          environmentId: "env-invalid-tunnel",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );
      expect(error).toMatchObject({ stage: "validate-tunnel-response" });
      if (error._tag === "ManagedEndpointProvisioningFailed") {
        expect(error.returnedTunnelId).toBeUndefined();
        expect(error.returnedTunnelName).toBeUndefined();
      }
    }).pipe(Effect.provide(providerLayer(tunnelClient)));
  });

  it.effect("renews operation ownership immediately around every provider side effect", () => {
    const events: Array<string> = [];
    const tunnelClient = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
      list: () =>
        Effect.sync(() => {
          events.push("provider:tunnel-list");
          return { result: [] };
        }),
      create: (request) =>
        Effect.sync(() => {
          events.push("provider:tunnel-create");
          return { id: "tunnel-id", name: request.name };
        }),
      putConfiguration: () => Effect.sync(() => events.push("provider:tunnel-configure")),
      getToken: () =>
        Effect.sync(() => {
          events.push("provider:tunnel-token");
          return "connector-token";
        }),
      delete: () => Effect.sync(() => events.push("provider:tunnel-delete")),
    });
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: () =>
        Effect.sync(() => {
          events.push("provider:dns-list");
          return [];
        }),
      createRecord: () =>
        Effect.sync(() => {
          events.push("provider:dns-create");
          return { id: "dns-id" };
        }),
      updateRecord: () => Effect.sync(() => events.push("provider:dns-update")),
      deleteRecord: () => Effect.sync(() => events.push("provider:dns-delete")),
    });
    const baseAllocations = makeAllocations();
    const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
      ...baseAllocations,
      renewOperation: () => Effect.sync(() => events.push("renew")),
    });
    const ownership: ManagedEndpointAllocations.ManagedEndpointOperation = {
      userId: "user-1",
      environmentId: "env-fenced",
      kind: "provision",
      generation: 7,
      ownerToken: "owner-7",
    };

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: ownership.userId,
        environmentId: ownership.environmentId,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        ownership,
      });

      const providerEvents = events
        .map((event, index) => ({ event, index }))
        .filter(({ event }) => event.startsWith("provider:"));
      // Six environment calls, then a lookup and a create per preview hostname.
      expect(providerEvents).toHaveLength(6 + 4 * 2);
      for (const { index } of providerEvents) {
        expect(events[index - 1]).toBe("renew");
        expect(events[index + 1]).toBe("renew");
      }
    }).pipe(Effect.provide(providerLayer(tunnelClient, dnsClient, allocations)));
  });

  it.effect("prevents a stale provision owner from configuring a tunnel after takeover", () => {
    const calls: TunnelCall[] = [];
    let renewals = 0;
    const baseAllocations = makeAllocations();
    const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
      ...baseAllocations,
      renewOperation: (operation) =>
        ++renewals === 5
          ? Effect.fail(
              new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
                operation: "renew-operation",
                stage: "ownership-lost",
                ...operation,
              }),
            )
          : Effect.void,
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* Effect.result(
        provider.provision({
          userId: "user-1",
          environmentId: "env-stale",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );
      expect(Result.isFailure(result)).toBe(true);
      expect(calls.map((call) => call.operation)).not.toContain("putConfiguration");
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(calls), makeDnsClient(), allocations)));
  });

  it.effect("cancels a provider call before its renewed ownership lease can expire", () =>
    Effect.gen(function* () {
      const started = yield* Deferred.make<void>();
      let remoteMutationCompleted = false;
      let takenOver = false;
      const tunnelClient = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
        list: () =>
          Deferred.succeed(started, undefined).pipe(
            Effect.andThen(Effect.sleep("5 minutes")),
            Effect.tap(() =>
              Effect.sync(() => {
                remoteMutationCompleted = true;
              }),
            ),
            Effect.as({ result: [] }),
          ),
        create: () => Effect.die("timed out list must stop provisioning"),
        putConfiguration: () => Effect.die("timed out list must stop provisioning"),
        getToken: () => Effect.die("timed out list must stop provisioning"),
        delete: () => Effect.die("unused"),
      });
      const baseAllocations = makeAllocations();
      const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
        ...baseAllocations,
        renewOperation: (operation) =>
          takenOver
            ? Effect.fail(
                new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
                  operation: "renew-operation",
                  stage: "ownership-lost",
                  ...operation,
                }),
              )
            : Effect.void,
      });
      const run = Effect.gen(function* () {
        const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
        return yield* provider.provision({
          userId: "user-1",
          environmentId: "env-provider-timeout",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        });
      }).pipe(Effect.provide(providerLayer(tunnelClient, makeDnsClient(), allocations)));

      const fiber = yield* Effect.forkChild(run);
      yield* Deferred.await(started);
      takenOver = true;
      yield* TestClock.adjust("4 minutes");
      const timedOut = fiber.pollUnsafe();
      expect(timedOut).toBeDefined();
      yield* TestClock.adjust("2 minutes");
      expect(remoteMutationCompleted).toBe(false);
      yield* Fiber.interrupt(fiber);
    }),
  );

  it.effect("maps prior allocation lookup failure before provider side effects", () => {
    const cause = new Error("allocation lookup unavailable");
    const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
      ...makeAllocations(),
      get: () =>
        Effect.fail(
          new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
            operation: "get",
            stage: "database-request",
            userId: "user-1",
            environmentId: "env-lookup-failure",
            cause,
          }),
        ),
    });
    const tunnelCalls: TunnelCall[] = [];

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.provision({
          userId: "user-1",
          environmentId: "env-lookup-failure",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );
      expect(error).toMatchObject({ stage: "reserve-allocation" });
      expect(tunnelCalls).toEqual([]);
    }).pipe(
      Effect.provide(providerLayer(makeTunnelClient(tunnelCalls), makeDnsClient(), allocations)),
    );
  });

  it.effect("prevents a stale deprovision owner from deleting remote resources", () => {
    const tunnelCalls: TunnelCall[] = [];
    const dnsCalls: DnsCall[] = [];
    let stale = false;
    const baseAllocations = makeAllocations();
    const allocations = ManagedEndpointAllocations.ManagedEndpointAllocations.of({
      ...baseAllocations,
      renewOperation: (operation) =>
        stale
          ? Effect.fail(
              new ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError({
                operation: "renew-operation",
                stage: "ownership-lost",
                ...operation,
              }),
            )
          : Effect.void,
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const key = { userId: "user-1", environmentId: "env-stale-delete" } as const;
      yield* provider.provision({
        ...key,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      stale = true;
      const result = yield* Effect.result(provider.deprovision(key));
      expect(Result.isFailure(result)).toBe(true);
      expect(tunnelCalls.map((call) => call.operation)).not.toContain("delete");
      expect(dnsCalls.map((call) => call.operation)).not.toContain("deleteRecord");
    }).pipe(
      Effect.provide(
        providerLayer(makeTunnelClient(tunnelCalls), makeDnsClient(dnsCalls), allocations),
      ),
    );
  });

  it.effect("provisions a Cloudflare tunnel endpoint and connector token", () => {
    const tunnelCalls: TunnelCall[] = [];
    const dnsCalls: DnsCall[] = [];
    const allocationCalls: AllocationCall[] = [];

    return Effect.gen(function* () {
      const hostname = expectedManagedHostname("env_ABC");
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      expect(result).toEqual({
        endpoint: {
          httpBaseUrl: `https://${hostname}/`,
          wsBaseUrl: `wss://${hostname}/ws`,
          providerKind: "cloudflare_tunnel",
        },
        runtime: {
          providerKind: "cloudflare_tunnel",
          connectorToken: "connector-token",
          tunnelId: "tunnel-id",
          tunnelName: expectedManagedTunnelName("env_ABC"),
          hostname,
          previewHostnames: expectedPreviewHostnames("env_ABC"),
        },
        endpointDisposition: "created",
      });
      expect(dnsCalls).toEqual([
        { operation: "listRecords", input: hostname },
        {
          operation: "createRecord",
          input: {
            type: "CNAME",
            name: hostname,
            content: "tunnel-id.cfargotunnel.com",
            ttl: 1,
            proxied: true,
          },
        },
      ]);
      expect(tunnelCalls.map((call) => call.operation)).toEqual([
        "list",
        "create",
        "putConfiguration",
        "getToken",
      ]);
      expect(tunnelCalls[2]?.input).toMatchObject({
        tunnelConfig: {
          ingress: [
            {
              hostname,
              service: "http://127.0.0.1:3773",
            },
            ...expectedPreviewHostnames("env_ABC").map((previewHostname) => ({
              hostname: previewHostname,
              service: "http://127.0.0.1:3773",
            })),
            { service: "http_status:404" },
          ],
        },
      });
      expect(tunnelCalls[0]?.input).toEqual({
        name: expectedManagedTunnelName("env_ABC"),
        isDeleted: false,
      });
      expect(
        allocationCalls
          .map((call) => call.operation)
          .filter((operation) => operation !== "renewOperation"),
      ).toEqual(["get", "reserve", "recordTunnel", "recordDns", "markReady"]);
    }).pipe(
      Effect.provide(
        providerLayer(
          makeTunnelClient(tunnelCalls),
          makeDnsClient(dnsCalls),
          makeAllocations(allocationCalls),
        ),
      ),
    );
  });

  it.effect("provisions preview hostnames with ingress before the catch-all", () => {
    const tunnelCalls: TunnelCall[] = [];
    const previewCalls: DnsCall[] = [];
    return Effect.gen(function* () {
      const hostname = expectedManagedHostname("env_ABC");
      const previews = expectedPreviewHostnames("env_ABC");
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      expect(result.runtime).toMatchObject({ hostname, previewHostnames: previews });
      expect(tunnelCalls[2]?.input).toMatchObject({
        tunnelConfig: {
          ingress: [
            { hostname, service: "http://127.0.0.1:3773" },
            ...previews.map((preview) => ({ hostname: preview, service: "http://127.0.0.1:3773" })),
            { service: "http_status:404" },
          ],
        },
      });
      expect(previewCalls.map((call) => [call.operation, call.input])).toEqual(
        previews.flatMap((preview) => [
          ["listRecords", preview],
          [
            "createRecord",
            {
              type: "CNAME",
              name: preview,
              content: "tunnel-id.cfargotunnel.com",
              ttl: 1,
              proxied: true,
            },
          ],
        ]),
      );
    }).pipe(
      Effect.provide(
        providerLayer(makeTunnelClient(tunnelCalls), makeDnsClient([], [], previewCalls)),
      ),
    );
  });

  it.effect("re-provisioning updates preview records instead of duplicating them", () => {
    const previewCalls: DnsCall[] = [];
    const layer = providerLayer(makePersistentTunnelClient(), makeDnsClient([], [], previewCalls));
    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const request = {
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      } as const;
      yield* provider.provision(request);
      previewCalls.length = 0;
      yield* provider.provision(request);
      expect(previewCalls.map((call) => call.operation)).toEqual(
        Array.from({ length: 4 }, () => ["listRecords", "updateRecord"]).flat(),
      );
    }).pipe(Effect.provide(layer));
  });

  it.effect("deprovisions preview records before the environment record and tunnel", () => {
    const previewCalls: DnsCall[] = [];
    const dnsCalls: DnsCall[] = [];
    const tunnelCalls: TunnelCall[] = [];
    const layer = providerLayer(
      makePersistentTunnelClient(tunnelCalls),
      makeDnsClient(dnsCalls, [], previewCalls),
    );
    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const key = { userId: "user_ABC", environmentId: "env_ABC" } as const;
      yield* provider.provision({
        ...key,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      previewCalls.length = 0;
      dnsCalls.length = 0;
      yield* provider.deprovision(key);
      expect(previewCalls.filter((call) => call.operation === "deleteRecord")).toHaveLength(4);
      expect(dnsCalls.map((call) => call.operation)).toEqual(["deleteRecord"]);
      expect(tunnelCalls.at(-1)?.operation).toBe("delete");
    }).pipe(Effect.provide(layer));
  });

  it.effect("maps preview record failures to their own stages", () => {
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "list-records",
      cause: new Error("preview list failed"),
    });
    const failingPreviews = (
      base: ManagedEndpointProvider.ManagedEndpointDnsClient["Service"],
    ): ManagedEndpointProvider.ManagedEndpointDnsClient["Service"] => ({
      ...base,
      listRecords: (hostname: string) =>
        PREVIEW_HOSTNAME.test(hostname) ? Effect.fail(failure) : base.listRecords(hostname),
    });
    const allocation: ManagedEndpointAllocations.ManagedEndpointAllocation = {
      userId: "user_ABC",
      environmentId: "env_ABC",
      hostname: expectedManagedHostname("env_ABC"),
      tunnelId: "tunnel-id",
      tunnelName: "tunnel-name",
      dnsRecordId: "dns-id",
      readyAt: "2026-01-01T00:00:00.000Z",
    };
    return Effect.gen(function* () {
      const provisionError = yield* Effect.flip(
        Effect.flatMap(ManagedEndpointProvider.ManagedEndpointProvider, (provider) =>
          provider.provision({
            userId: "user_ABC",
            environmentId: "env_ABC",
            origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
          }),
        ).pipe(Effect.provide(providerLayer(makeTunnelClient(), failingPreviews(makeDnsClient())))),
      );
      const deprovisionError = yield* Effect.flip(
        Effect.flatMap(ManagedEndpointProvider.ManagedEndpointProvider, (provider) =>
          provider.deprovision({ userId: "user_ABC", environmentId: "env_ABC" }),
        ).pipe(
          Effect.provide(
            providerLayer(makeTunnelClient(), failingPreviews(makeDnsClient()), {
              ...makeAllocations(),
              claimForOperation: () => Effect.succeed(allocation),
            }),
          ),
        ),
      );
      expect(provisionError).toMatchObject({ stage: "ensure-preview-dns-record" });
      expect(deprovisionError).toMatchObject({ stage: "delete-preview-dns-record" });
    });
  });

  it.effect("uses stage-scoped stable names without leaking unusual environment ids", () => {
    const tunnelCalls: TunnelCall[] = [];

    return Effect.gen(function* () {
      const environmentId = "ENV With Spaces/../Symbols!" + "x".repeat(80);
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      const requestedName = (
        tunnelCalls.find((call) => call.operation === "list")?.input as
          | { readonly name?: string }
          | undefined
      )?.name;
      expect(requestedName).toMatch(/^bibcoderelay-managedendpoint-dev-julius-[a-f0-9]{16}$/);
      const configBody = (
        tunnelCalls.find((call) => call.operation === "putConfiguration")?.input as
          | { readonly tunnelConfig?: unknown }
          | undefined
      )?.tunnelConfig;
      expect(configBody).toMatchObject({
        ingress: [
          {
            hostname: expect.stringMatching(/^dev-julius-[a-f0-9]{16}\.bibcode\.test$/),
          },
          ...[1, 2, 3, 4].map((slot) => ({
            hostname: expect.stringMatching(
              new RegExp(`^dev-julius-[a-f0-9]{16}-p${slot}\\.bibcode\\.test$`, "u"),
            ),
          })),
          { service: "http_status:404" },
        ],
      });
      const hostname = (
        configBody as
          | {
              readonly ingress?: readonly [{ readonly hostname?: unknown }, unknown];
            }
          | undefined
      )?.ingress?.[0]?.hostname;
      expect(typeof hostname === "string" ? hostname.split(".")[0]?.length : 0).toBeLessThanOrEqual(
        63,
      );
      expect(tunnelCalls.find((call) => call.operation === "create")?.input).toMatchObject({
        name: requestedName,
        configSrc: "cloudflare",
      });
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(tunnelCalls))));
  });

  it.effect("formats IPv6 loopback origins as valid Cloudflare ingress service URLs", () => {
    const tunnelCalls: TunnelCall[] = [];

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env-ipv6",
        origin: { localHttpHost: "::1", localHttpPort: 3773 },
      });

      expect(
        tunnelCalls.find((call) => call.operation === "putConfiguration")?.input,
      ).toMatchObject({
        tunnelConfig: {
          // The environment rule and every preview rule share the origin service.
          ingress: [
            ...Array.from({ length: 5 }, () => ({ service: "http://[::1]:3773" })),
            { service: "http_status:404" },
          ],
        },
      });
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(tunnelCalls))));
  });

  it.effect("rejects non-loopback managed endpoint origins before calling Cloudflare", () => {
    const dnsCalls: DnsCall[] = [];

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* Effect.result(
        provider.provision({
          userId: "user_ABC",
          environmentId: "env_ABC",
          origin: { localHttpHost: "192.168.1.10", localHttpPort: 3773 },
        }),
      );

      expect(dnsCalls).toHaveLength(0);
      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.failure).toMatchObject({
          _tag: "ManagedEndpointOriginNotAllowed",
          userId: "user_ABC",
          environmentId: "env_ABC",
          host: "192.168.1.10",
          port: 3773,
        });
      }
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(), makeDnsClient(dnsCalls))));
  });

  it.effect("rejects invalid managed endpoint origin ports before calling Cloudflare", () => {
    const dnsCalls: DnsCall[] = [];

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const result = yield* Effect.result(
        provider.provision({
          userId: "user_ABC",
          environmentId: "env_ABC",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 65_536 },
        }),
      );

      expect(dnsCalls).toHaveLength(0);
      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.failure._tag).toBe("ManagedEndpointOriginNotAllowed");
      }
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(), makeDnsClient(dnsCalls))));
  });

  it.effect("reconciles an existing same-host DNS record through the DNS client", () => {
    const dnsCalls: DnsCall[] = [];
    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      expect(dnsCalls.map((call) => call.operation)).toEqual(["listRecords", "updateRecord"]);
      expect(dnsCalls[1]?.input).toMatchObject({ dnsRecordId: "existing-record-id" });
    }).pipe(
      Effect.provide(
        providerLayer(makeTunnelClient(), makeDnsClient(dnsCalls, [{ id: "existing-record-id" }])),
      ),
    );
  });

  it.effect("reuses checkpointed resources when provisioning is retried", () => {
    const tunnelCalls: TunnelCall[] = [];
    const dnsCalls: DnsCall[] = [];
    const allocationCalls: AllocationCall[] = [];
    const layer = providerLayer(
      makePersistentTunnelClient(tunnelCalls),
      makeDnsClient(dnsCalls),
      makeAllocations(allocationCalls),
    );

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const request = {
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      } as const;
      yield* provider.provision(request);
      yield* provider.provision(request);

      expect(tunnelCalls.map((call) => call.operation)).toEqual([
        "list",
        "create",
        "putConfiguration",
        "getToken",
        "list",
        "putConfiguration",
        "getToken",
      ]);
      expect(dnsCalls.map((call) => call.operation)).toEqual([
        "listRecords",
        "createRecord",
        "updateRecord",
      ]);
      expect(
        allocationCalls
          .map((call) => call.operation)
          .filter((operation) => operation !== "renewOperation"),
      ).toEqual([
        "get",
        "reserve",
        "recordTunnel",
        "recordDns",
        "markReady",
        "get",
        "reserve",
        "recordTunnel",
        "recordDns",
        "markReady",
      ]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("recreates a checkpointed DNS record when it was removed externally", () => {
    const dnsCalls: DnsCall[] = [];
    const allocationCalls: AllocationCall[] = [];
    const dnsClient = makeDnsClient(dnsCalls);
    const layer = providerLayer(
      makePersistentTunnelClient(),
      dnsClient,
      makeAllocations(allocationCalls),
    );

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const request = {
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      } as const;
      yield* provider.provision(request);
      yield* dnsClient.deleteRecord("created-record-id");
      yield* provider.provision(request);

      expect(dnsCalls.map((call) => call.operation)).toEqual([
        "listRecords",
        "createRecord",
        "deleteRecord",
        "updateRecord",
        "listRecords",
        "createRecord",
      ]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("does not hide non-not-found checkpoint update failures", () => {
    const dnsCalls: DnsCall[] = [];
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "update-record",
      dnsRecordId: "created-record-id",
      cause: new Error("Cloudflare DNS unavailable"),
    });
    let records: ReadonlyArray<{ readonly id: string }> = [];
    // Preview records are always created fresh here; only the checkpointed
    // environment record exercises the failing update.
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: (hostname) =>
        Effect.sync(() => {
          if (PREVIEW_HOSTNAME.test(hostname)) return [];
          dnsCalls.push({ operation: "listRecords", input: hostname });
          return records;
        }),
      createRecord: (request) =>
        Effect.sync(() => {
          if (PREVIEW_HOSTNAME.test(request.name)) return { id: `preview:${request.name}` };
          dnsCalls.push({ operation: "createRecord", input: request });
          const record = { id: "created-record-id" };
          records = [record];
          return record;
        }),
      updateRecord: (dnsRecordId, request) =>
        Effect.sync(() => {
          dnsCalls.push({ operation: "updateRecord", input: { dnsRecordId, request } });
        }).pipe(Effect.andThen(Effect.fail(failure))),
      deleteRecord: () => Effect.void,
    });
    const layer = providerLayer(makePersistentTunnelClient(), dnsClient, makeAllocations());

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const request = {
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      } as const;
      yield* provider.provision(request);
      const error = yield* Effect.flip(provider.provision(request));

      expect(error).toMatchObject({
        _tag: "ManagedEndpointProvisioningFailed",
        stage: "ensure-dns-record",
        userId: "user_ABC",
        environmentId: "env_ABC",
      });
      expect(dnsCalls.map((call) => call.operation)).toEqual([
        "listRecords",
        "createRecord",
        "updateRecord",
      ]);
    }).pipe(Effect.provide(layer));
  });

  it.effect(
    "deprovisions checkpointed DNS and tunnel resources before removing the allocation",
    () => {
      const tunnelCalls: TunnelCall[] = [];
      const dnsCalls: DnsCall[] = [];
      const allocationCalls: AllocationCall[] = [];
      const layer = providerLayer(
        makePersistentTunnelClient(tunnelCalls),
        makeDnsClient(dnsCalls),
        makeAllocations(allocationCalls),
      );

      return Effect.gen(function* () {
        const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
        const key = { userId: "user_ABC", environmentId: "env_ABC" } as const;
        yield* provider.provision({
          ...key,
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        });
        yield* provider.deprovision(key);

        expect(dnsCalls.map((call) => call.operation)).toEqual([
          "listRecords",
          "createRecord",
          "deleteRecord",
        ]);
        expect(tunnelCalls.map((call) => call.operation)).toEqual([
          "list",
          "create",
          "putConfiguration",
          "getToken",
          "delete",
        ]);
        expect(
          allocationCalls
            .map((call) => call.operation)
            .filter((operation) => operation !== "renewOperation"),
        ).toEqual(["get", "reserve", "recordTunnel", "recordDns", "markReady", "get", "remove"]);
      }).pipe(Effect.provide(layer));
    },
  );

  it.effect("treats an absent allocation as already deprovisioned", () => {
    const tunnelCalls: TunnelCall[] = [];
    const dnsCalls: DnsCall[] = [];
    const allocationCalls: AllocationCall[] = [];
    const layer = providerLayer(
      makePersistentTunnelClient(tunnelCalls),
      makeDnsClient(dnsCalls),
      makeAllocations(allocationCalls),
    );

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const key = { userId: "user_ABC", environmentId: "env_ABC" } as const;
      yield* provider.deprovision(key);

      expect(tunnelCalls).toEqual([]);
      expect(dnsCalls).toEqual([]);
      expect(allocationCalls).toEqual([{ operation: "get", input: key }]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("keeps the allocation when tunnel cleanup fails so unlink can retry", () => {
    const allocationCalls: AllocationCall[] = [];
    const tunnelCalls: TunnelCall[] = [];
    let deleteAttempts = 0;
    const failure = new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
      operation: "delete",
      tunnelId: "tunnel-id",
      cause: "Cloudflare tunnel deletion failed",
    });
    const tunnels = makePersistentTunnelClient(tunnelCalls);
    const tunnelClient = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
      ...tunnels,
      delete: (tunnelId) =>
        Effect.gen(function* () {
          tunnelCalls.push({ operation: "delete", input: tunnelId });
          deleteAttempts++;
          if (deleteAttempts === 1) {
            return yield* failure;
          }
        }),
    });
    const layer = providerLayer(tunnelClient, makeDnsClient(), makeAllocations(allocationCalls));

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const key = { userId: "user_ABC", environmentId: "env_ABC" } as const;
      yield* provider.provision({
        ...key,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      const first = yield* Effect.result(provider.deprovision(key));
      expect(first._tag).toBe("Failure");
      if (first._tag === "Failure") {
        expect(first.failure).toMatchObject({
          _tag: "ManagedEndpointDeprovisioningFailed",
          stage: "delete-tunnel",
          userId: key.userId,
          environmentId: key.environmentId,
          tunnelId: "tunnel-id",
        });
        expect(first.failure.cause).toBe(failure);
      }
      yield* provider.deprovision(key);

      expect(
        allocationCalls
          .map((call) => call.operation)
          .filter((operation) => operation !== "renewOperation"),
      ).toEqual([
        "get",
        "reserve",
        "recordTunnel",
        "recordDns",
        "markReady",
        "get",
        "get",
        "remove",
      ]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("treats already deleted remote resources as successfully deprovisioned", () => {
    const allocationCalls: AllocationCall[] = [];
    const notFound = { _tag: "NotFound" } as const;
    const tunnelClient = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
      ...makeTunnelClient(),
      delete: () =>
        Effect.fail(
          new ManagedEndpointProvider.ManagedEndpointTunnelClientError({
            operation: "delete",
            tunnelId: "tunnel-id",
            cause: notFound,
          }),
        ),
    });
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      ...makeDnsClient(),
      deleteRecord: () =>
        Effect.fail(
          new ManagedEndpointProvider.ManagedEndpointDnsClientError({
            operation: "delete-record",
            dnsRecordId: "created-record-id",
            cause: notFound,
          }),
        ),
    });
    const layer = providerLayer(tunnelClient, dnsClient, makeAllocations(allocationCalls));

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const key = { userId: "user_ABC", environmentId: "env_ABC" } as const;
      yield* provider.provision({
        ...key,
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      yield* provider.deprovision(key);

      expect(allocationCalls.map((call) => call.operation)).toContain("remove");
    }).pipe(Effect.provide(layer));
  });

  it.effect("scopes managed endpoint resources by user", () => {
    const tunnelCalls: TunnelCall[] = [];

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env_shared",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });
      yield* provider.provision({
        userId: "user_DEF",
        environmentId: "env_shared",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      expect(
        tunnelCalls.filter((call) => call.operation === "list").map((call) => call.input),
      ).toEqual([
        { name: expectedManagedTunnelName("env_shared", "user_ABC"), isDeleted: false },
        { name: expectedManagedTunnelName("env_shared", "user_DEF"), isDeleted: false },
      ]);
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(tunnelCalls))));
  });

  it.effect("recovers when DNS creation reports failure after the record became visible", () => {
    const dnsCalls: DnsCall[] = [];
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "create-record",
      hostname: expectedManagedHostname("env_ABC"),
      cause: "ambiguous Cloudflare DNS response",
    });
    let records: ReadonlyArray<{ readonly id: string }> = [];
    // Only the environment record's create is ambiguous; preview records are created normally.
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: (hostname) =>
        Effect.sync(() => {
          if (PREVIEW_HOSTNAME.test(hostname)) return [];
          dnsCalls.push({ operation: "listRecords", input: hostname });
          return records;
        }),
      createRecord: (request) =>
        PREVIEW_HOSTNAME.test(request.name)
          ? Effect.succeed({ id: `preview:${request.name}` })
          : Effect.gen(function* () {
              dnsCalls.push({ operation: "createRecord", input: request });
              records = [{ id: "created-record-id" }];
              return yield* failure;
            }),
      updateRecord: (dnsRecordId, request) =>
        Effect.sync(() => {
          dnsCalls.push({ operation: "updateRecord", input: { dnsRecordId, request } });
        }),
      deleteRecord: (dnsRecordId) =>
        Effect.sync(() => {
          dnsCalls.push({ operation: "deleteRecord", input: dnsRecordId });
        }),
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      yield* provider.provision({
        userId: "user_ABC",
        environmentId: "env_ABC",
        origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
      });

      expect(dnsCalls.map((call) => call.operation)).toEqual([
        "listRecords",
        "createRecord",
        "listRecords",
        "updateRecord",
      ]);
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(), dnsClient)));
  });

  it.effect("reports mismatched tunnel responses without manufacturing a cause", () => {
    const dnsCalls: DnsCall[] = [];
    const tunnelClient = ManagedEndpointProvider.ManagedEndpointTunnelClient.of({
      ...makeTunnelClient(),
      create: () => Effect.succeed({ id: "returned-tunnel-id", name: "unexpected-tunnel" }),
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.provision({
          userId: "user_ABC",
          environmentId: "env_ABC",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );

      expect(error).toMatchObject({
        _tag: "ManagedEndpointProvisioningFailed",
        stage: "validate-tunnel-response",
        userId: "user_ABC",
        environmentId: "env_ABC",
        hostname: expectedManagedHostname("env_ABC"),
        tunnelName: expectedManagedTunnelName("env_ABC"),
        returnedTunnelId: "returned-tunnel-id",
        returnedTunnelName: "unexpected-tunnel",
      });
      if (error._tag === "ManagedEndpointProvisioningFailed") {
        expect(error.cause).toBeUndefined();
      }
      expect(dnsCalls).toHaveLength(0);
    }).pipe(Effect.provide(providerLayer(tunnelClient, makeDnsClient(dnsCalls))));
  });

  it.effect("fails provisioning when the DNS client fails", () => {
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "list-records",
      hostname: expectedManagedHostname("env_ABC"),
      cause: "Cloudflare DNS failure",
    });
    const dnsClient = ManagedEndpointProvider.ManagedEndpointDnsClient.of({
      listRecords: () => Effect.fail(failure),
      createRecord: () => Effect.die("unused"),
      updateRecord: () => Effect.die("unused"),
      deleteRecord: () => Effect.die("unused"),
    });

    return Effect.gen(function* () {
      const provider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
      const error = yield* Effect.flip(
        provider.provision({
          userId: "user_ABC",
          environmentId: "env_ABC",
          origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 },
        }),
      );

      expect(error).toMatchObject({
        _tag: "ManagedEndpointProvisioningFailed",
        stage: "ensure-dns-record",
        userId: "user_ABC",
        environmentId: "env_ABC",
        hostname: expectedManagedHostname("env_ABC"),
        tunnelName: expectedManagedTunnelName("env_ABC"),
        tunnelId: "tunnel-id",
      });
      if (error._tag === "ManagedEndpointProvisioningFailed") {
        expect(error.cause).toBe(failure);
      }
    }).pipe(Effect.provide(providerLayer(makeTunnelClient(), dnsClient)));
  });
});
