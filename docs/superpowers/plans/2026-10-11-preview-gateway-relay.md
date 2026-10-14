# Preview gateway over BiBCode Connect — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relay-connected clients open server-loopback dev servers through the preview gateway at `https://<env>-pN.<zone>`, with the Connect tunnel restored after a server restart.

**Architecture:** The relay provisions 4 fixed preview hostnames per environment (CNAME + ingress to the server's existing loopback origin) and returns them in the runtime config. The server publishes those names from its managed-endpoint runtime into a shared `RelayHosts` handle; gateway admission recognizes relay callers by the endpoint hostname and assigns each relay target a slot; an outermost main-port middleware hands requests for a slot hostname to that target's existing gateway handler, HTTPS-facing. The web resolver gets a `relay` path that bootstraps on the returned origin, and the primary client re-links once per session when its server lacks preview hostnames.

**Tech Stack:** Rust (Axum, Hyper 1, Tokio), TypeScript (Effect Schema, Effect, React 19, Vitest via Vite+), Cloudflare Tunnel/DNS through Alchemy bindings.

**Spec:** `docs/superpowers/specs/2026-10-11-preview-gateway-relay-design.md`

## Global Constraints

- 4 preview slots per environment: `RELAY_PREVIEW_SLOTS = 4` (server) and `MANAGED_ENDPOINT_PREVIEW_SLOTS = 4` (relay).
- Preview hostname = environment hostname's first label + `-p1` … `-p4`, same zone; label ≤ 63 chars, hash kept.
- Relay-routed gateway cookie: `bibcode-gw-<port>=<id>; HttpOnly; Secure; SameSite=Lax; Path=/`.
- New `PreviewGatewayError.reason` literals: `relay-not-provisioned`, `relay-busy`.
- New optional fields: `RelayManagedEndpointRuntimeConfig.hostname`, `.previewHostnames`; `PreviewGatewayOpenResult.origin`; `EnvironmentCloudLinkStateResult.managedEndpoint`, `.previewHostnames`.
- Copy (verbatim):
  - not provisioned: `Previews over BiBCode Connect aren't set up for ${label} yet. Open BiBCode on ${label} to finish setting them up.`
  - busy: `Too many previews of ${label} are open over BiBCode Connect. Close one, then try again.`
  - browser-mode frame: `${label} is reached through BiBCode Connect, so its previews can't show inside this page. Click the address in the chat or terminal to open it in a new tab.`
- No per-task commits: the user commits once at the end (Task 11), after a Codex review.
- Long Rust commands run memory-capped: `systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo …`, with `CARGO_TARGET_DIR=$PWD/target`.
- `vp` is `node scripts/run-local-vp.mjs` (repo root) or `node ../../scripts/run-local-vp.mjs` (package dir).

## Review Focus

1. A `Host` header with a port or trailing dot (`env-p1.zone:443`, `ENV-P1.zone.`) must still match its slot — pinned in Task 4 (`host_name` tests).
2. A slot taken over by another target must answer the old tab's cookie with `401`, never serve the new upstream under the old session — pinned in Task 7 (takeover test).
3. A relay config that changes only hostnames must not restart a running connector — pinned in Task 5 (same pid test).
4. A WebSocket upgrade (Vite HMR) through the main-port router must pass the https Origin rule and reach the upstream with `Origin: http://localhost:<port>` — pinned in Task 7.
5. Browser mode must never frame a relay preview (the frame would show a 401) — pinned in Task 8.

---

### Task 1: Contracts

**Files:**
- Modify: `packages/contracts/src/relay.ts:37-42`
- Modify: `packages/contracts/src/preview.ts:212-218,338-357`
- Modify: `packages/contracts/src/environmentHttp.ts:331-336`
- Test: the contracts package's existing schema tests plus the regenerated Rust RPC fixtures (`vp run check:contracts`)

**Interfaces:**
- Produces: `RelayManagedEndpointRuntimeConfig.{hostname?, previewHostnames?}`, `PreviewGatewayOpenResult.origin?`, reasons `relay-not-provisioned` | `relay-busy`, `EnvironmentCloudLinkStateResult.{managedEndpoint?, previewHostnames?}`.

- [ ] **Step 1: Extend the schemas**

```ts
// relay.ts
export const RelayManagedEndpointRuntimeConfig = Schema.Struct({
  providerKind: RelayManagedEndpointProviderKind,
  connectorToken: TrimmedNonEmptyString,
  tunnelId: Schema.optional(TrimmedNonEmptyString),
  tunnelName: Schema.optional(TrimmedNonEmptyString),
  /** The managed endpoint's hostname; relay connections arrive under it. */
  hostname: Schema.optional(TrimmedNonEmptyString),
  /** The environment's fixed preview hostnames, in slot order. */
  previewHostnames: Schema.optional(Schema.Array(TrimmedNonEmptyString)),
});
```

```ts
// preview.ts
/**
 * `capability` bootstraps one gateway session on `gatewayPort` before `expiresAtMs`.
 * `origin` is set for a caller that came through BiBCode Connect: the preview
 * hostname to bootstrap on instead of the server's address.
 */
export const PreviewGatewayOpenResult = Schema.Struct({
  gatewayPort: PortSchema,
  capability: TrimmedNonEmptyString,
  expiresAtMs: Schema.Finite,
  origin: Schema.optional(TrimmedNonEmptyString),
});
```

Add to the doc comment of `PreviewGatewayError`: "`relay-not-provisioned` (a BiBCode Connect caller whose environment has no preview hostnames yet), `relay-busy` (every preview hostname serves a target with live connections)", and to its `reason` literals:

```ts
    reason: Schema.Literals([
      "not-admitted",
      "https-unsupported",
      "no-upstream",
      "not-reachable",
      "relay-not-provisioned",
      "relay-busy",
      "unavailable",
    ]),
```

```ts
// environmentHttp.ts
export const EnvironmentCloudLinkStateResult = Schema.Struct({
  linked: Schema.Boolean,
  cloudUserId: Schema.NullOr(Schema.String),
  relayUrl: Schema.NullOr(Schema.String),
  relayIssuer: Schema.NullOr(Schema.String),
  /** The stored link runs a managed tunnel. Absent from servers that predate it. */
  managedEndpoint: Schema.optional(Schema.Boolean),
  /** That tunnel's config names preview hostnames. Absent from servers that predate it. */
  previewHostnames: Schema.optional(Schema.Boolean),
});
```

- [ ] **Step 2: Regenerate fixtures and typecheck**

Run: `node scripts/run-local-vp.mjs run check:contracts`
Expected: typecheck passes; Rust RPC fixtures regenerate with the new reason literals and optional field (no RPC count change). Review `git diff --stat` for only fixture and schema files.

---

### Task 2: Relay — preview hostname derivation

**Files:**
- Modify: `infra/relay/src/deploymentConfig.ts`
- Test: `infra/relay/src/deploymentConfig.test.ts` (create if absent; otherwise add a `describe`)

**Interfaces:**
- Produces: `MANAGED_ENDPOINT_PREVIEW_SLOTS = 4`, `managedEndpointPreviewHostnames(hostname: string): ReadonlyArray<string>`.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it } from "vite-plus/test";

import { managedEndpointHostname, managedEndpointPreviewHostnames } from "./deploymentConfig.ts";

describe("managedEndpointPreviewHostnames", () => {
  it("appends -p1 … -p4 to the environment label in the same zone", () => {
    expect(managedEndpointPreviewHostnames("dev-julius-0123456789abcdef.bibcode.test")).toEqual([
      "dev-julius-0123456789abcdef-p1.bibcode.test",
      "dev-julius-0123456789abcdef-p2.bibcode.test",
      "dev-julius-0123456789abcdef-p3.bibcode.test",
      "dev-julius-0123456789abcdef-p4.bibcode.test",
    ]);
  });

  it("shortens a long stage prefix and keeps the hash within 63 characters", () => {
    const hostname = managedEndpointHostname("s".repeat(80), "bibcode.test", "0123456789abcdef");
    const previews = managedEndpointPreviewHostnames(hostname);
    for (const [index, preview] of previews.entries()) {
      const label = preview.split(".")[0]!;
      expect(label.length).toBeLessThanOrEqual(63);
      expect(label.endsWith(`-0123456789abcdef-p${index + 1}`)).toBe(true);
      expect(preview.endsWith(".bibcode.test")).toBe(true);
    }
  });
});
```

- [ ] **Step 2: Run it to see it fail**

Run: `cd infra/relay && node ../../scripts/run-local-vp.mjs test run src/deploymentConfig.test.ts`
Expected: FAIL, `managedEndpointPreviewHostnames` is not exported.

- [ ] **Step 3: Implement**

```ts
/** Preview hostnames provisioned per managed endpoint. */
export const MANAGED_ENDPOINT_PREVIEW_SLOTS = 4;

/**
 * The managed endpoint's fixed preview hostnames: its first label plus `-p1` …
 * `-p4`, in the same zone. A label that would exceed the DNS limit shortens its
 * stage prefix and keeps the hash. Environment labels end in the hex hash, so
 * these never collide with another environment's hostname.
 */
export function managedEndpointPreviewHostnames(hostname: string): ReadonlyArray<string> {
  const [label = "", ...zone] = hostname.split(".");
  const hash = label.slice(-MANAGED_ENDPOINT_HASH_LENGTH);
  const prefix = label.slice(0, -(MANAGED_ENDPOINT_HASH_LENGTH + 1));
  return Array.from({ length: MANAGED_ENDPOINT_PREVIEW_SLOTS }, (_, index) => {
    const slot = `-p${index + 1}`;
    const previewLabel =
      label.length + slot.length <= DNS_LABEL_MAX_LENGTH
        ? `${label}${slot}`
        : appendDnsSafeSuffix(prefix, `${hash}${slot}`);
    return [previewLabel, ...zone].join(".");
  });
}
```

- [ ] **Step 4: Run the test to see it pass** (same command). Expected: PASS.

---

### Task 3: Relay — provision and deprovision preview records

**Files:**
- Modify: `infra/relay/src/environments/ManagedEndpointProvider.ts` (stages, `provisionOwned`, `deprovisionOwned`)
- Test: `infra/relay/src/environments/ManagedEndpointProvider.test.ts`

**Interfaces:**
- Consumes: `managedEndpointPreviewHostnames` (Task 2), `RelayManagedEndpointRuntimeConfig.{hostname, previewHostnames}` (Task 1).
- Produces: provisioning stage `ensure-preview-dns-record`; deprovisioning stage `delete-preview-dns-record`; runtime config with `hostname` and `previewHostnames`.

- [ ] **Step 1: Split preview DNS calls in the mock and write the failing tests**

Replace `makeDnsClient` so preview hostnames have their own records and call log, keeping every existing main-record assertion valid:

```ts
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
```

Add a helper next to `expectedManagedHostname`:

```ts
function expectedPreviewHostnames(environmentId: string, userId = "user_ABC"): string[] {
  const [label, ...zone] = expectedManagedHostname(environmentId, userId).split(".");
  return [1, 2, 3, 4].map((slot) => [`${label}-p${slot}`, ...zone].join("."));
}
```

New tests (inside `describe("ManagedEndpointProvider")`):

```ts
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
    const order: string[] = [];
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
      yield* provider.provision({ ...key, origin: { localHttpHost: "127.0.0.1", localHttpPort: 3773 } });
      previewCalls.length = 0;
      dnsCalls.length = 0;
      yield* provider.deprovision(key);
      order.push(...previewCalls.map((call) => `preview:${call.operation}`));
      expect(order.filter((step) => step === "preview:deleteRecord")).toHaveLength(4);
      expect(dnsCalls.map((call) => call.operation)).toEqual(["deleteRecord"]);
      expect(tunnelCalls.at(-1)?.operation).toBe("delete");
    }).pipe(Effect.provide(layer));
  });

  it.effect("maps preview record failures to their own stages", () => {
    const failure = new ManagedEndpointProvider.ManagedEndpointDnsClientError({
      operation: "list-records",
      cause: new Error("preview list failed"),
    });
    const failingPreviews = (base: ManagedEndpointProvider.ManagedEndpointDnsClient["Service"]) => ({
      ...base,
      listRecords: (hostname: string) =>
        /-p[1-4]\./u.test(hostname) ? Effect.fail(failure) : base.listRecords(hostname),
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
```

Update the exact `result` assertion in "provisions a Cloudflare tunnel endpoint and connector token" so `runtime` also carries `hostname` and `previewHostnames: expectedPreviewHostnames("env_ABC")`, and its ingress expectation lists the four preview rules before the 404.

- [ ] **Step 2: Run to see failures**

Run: `cd infra/relay && node ../../scripts/run-local-vp.mjs test run src/environments/ManagedEndpointProvider.test.ts`
Expected: the four new tests and the updated assertion FAIL; previously passing tests still pass.

- [ ] **Step 3: Implement**

In `ManagedEndpointProvisioningStage` add `"ensure-preview-dns-record"` after `"record-dns"`; in `ManagedEndpointDeprovisioningStage` add `"delete-preview-dns-record"` before `"delete-dns-record"`. Import `managedEndpointPreviewHostnames`.

In `provisionOwned`, right after `const { hostname, tunnelName } = allocation;`:

```ts
    const previewHostnames = managedEndpointPreviewHostnames(hostname);
```

Replace the ingress in `putConfiguration`:

```ts
        ingress: [
          { hostname, service: formatOriginService(input.origin) },
          ...previewHostnames.map((previewHostname) => ({
            hostname: previewHostname,
            service: formatOriginService(input.origin),
          })),
          { service: "http_status:404" },
        ],
```

After the `recordDns` step and before `getToken`:

```ts
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
```

Return `runtime: { providerKind: "cloudflare_tunnel", connectorToken, tunnelId: tunnel.id, tunnelName: tunnel.name, hostname, previewHostnames }`.

In `deprovisionOwned`, after the `allocation === null` early return and before deleting `allocation.dnsRecordId`:

```ts
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
```

- [ ] **Step 4: Update the Cloudflare-binding tests whose mocks ignore hostnames**

- "adapts Cloudflare tunnel and DNS bindings…": keep records per name (`Map<string, {id, name}>`, ids `dns-<name>`; `listDnsRecords({search})` returns `[...map.values()]`; delete removes by id). Expected `dnsCalls`: `["list-records", "create-record", ...4 × ["list-records", "create-record"], ...4 × ["list-records", "delete-record"], "delete-record"]`.
- "filters and normalizes Cloudflare DNS records…": return the fixture records only when `request.search` has no `-p` slot suffix (`/-p[1-4]\./u`), else `[]`; make `createDnsRecord` succeed for preview names (`Effect.succeed({ id: "preview-id" })`). Assertions unchanged.
- "recovers a Cloudflare DNS create collision…": expect `listCalls` to be `6` (main lookup, main retry lookup, one lookup per preview hostname, each preview finding the winner and updating it).

- [ ] **Step 5: Run the whole provider suite**

Run: `cd infra/relay && node ../../scripts/run-local-vp.mjs test run`
Expected: PASS (all relay tests, including `ManagedEndpointProvider.test.ts` and `deploymentConfig.test.ts`).

---

### Task 4: Server — `RelayHosts` and `RelaySlots`

**Files:**
- Create: `apps/server/src/preview/gateway/relay.rs`
- Modify: `apps/server/src/preview/gateway/mod.rs` (add `pub mod relay;`)
- Modify: `apps/server/src/preview/gateway/proxy.rs` (add `TargetRoute`, `RunningTarget.route`, `pub type ProxyBody`)

**Interfaces:**
- Produces:
  - `pub const RELAY_PREVIEW_SLOTS: usize = 4;`
  - `pub struct RelayPreviewHosts { pub hostname: String, pub previews: Vec<String> }`
  - `#[derive(Clone, Default)] pub struct RelayHosts` with `publish(&self, Option<RelayPreviewHosts>)`, `current(&self) -> Option<RelayPreviewHosts>`, `pub(crate) fn preview_slot(&self, host: &str) -> Option<usize>`
  - `pub(crate) fn is_relay_reach(hosts: &RelayPreviewHosts, local_ip: IpAddr, host: Option<&str>) -> bool`
  - `pub(crate) struct RelaySlots` with `assign(&self, &TargetRoute, available: usize) -> Option<usize>`, `route(&self, usize) -> Option<TargetRoute>`
  - In `proxy.rs`: `#[derive(Clone)] pub struct TargetRoute(Arc<Listener>)` with `same`, `is_closed`, `is_idle`, `last_activity_ms`, and (Task 7) `serve_https`.

- [ ] **Step 1: Add `TargetRoute` in `proxy.rs`**

```rust
pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// One running target's handler, for requests the main port routes to it (BiBCode
/// Connect preview hostnames). Closed once the target closes or drains.
#[derive(Clone)]
pub struct TargetRoute(Arc<Listener>);

impl TargetRoute {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.shutdown.is_cancelled() || self.0.draining.is_cancelled()
    }

    /// No open connection, exchange, or tunnel.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.0.active.load(Ordering::Relaxed) == 0
    }

    #[must_use]
    pub fn last_activity_ms(&self) -> u64 {
        self.0.last_activity_ms.load(Ordering::Relaxed)
    }
}
```

Add `pub route: TargetRoute` to `RunningTarget` and set `route: TargetRoute(state.clone())` in `start_target`. Remove the old private `type ProxyBody` line.

- [ ] **Step 2: Write `relay.rs` with its failing tests**

```rust
//! BiBCode Connect (relay) reach for the preview gateway. The Connect tunnel runtime
//! publishes the managed endpoint's hostname and its fixed preview hostnames
//! ([`RelayHosts`]). Gateway admission recognizes a relay caller by that hostname
//! ([`is_relay_reach`]) and gives each relay target one preview hostname
//! ([`RelaySlots`]); the main port serves those hostnames through
//! `PreviewGateway::route_relay`.

use std::{
    net::IpAddr,
    sync::{Arc, Mutex, PoisonError, RwLock},
};

use super::proxy::TargetRoute;

/// Preview hostnames per environment; published names beyond this are ignored.
pub const RELAY_PREVIEW_SLOTS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayPreviewHosts {
    /// The managed endpoint's hostname; relay RPC connections arrive under it.
    pub hostname: String,
    /// At most [`RELAY_PREVIEW_SLOTS`] preview hostnames, in slot order.
    pub previews: Vec<String>,
}

/// The current [`RelayPreviewHosts`], written by the Connect tunnel runtime and read by
/// the gateway. `None` while no managed tunnel runs.
#[derive(Clone, Default)]
pub struct RelayHosts(Arc<RwLock<Option<RelayPreviewHosts>>>);

impl RelayHosts {
    pub fn publish(&self, hosts: Option<RelayPreviewHosts>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = hosts;
    }

    #[must_use]
    pub fn current(&self) -> Option<RelayPreviewHosts> {
        self.0.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The slot whose preview hostname `host` (a `Host` header value) names.
    pub(crate) fn preview_slot(&self, host: &str) -> Option<usize> {
        let name = host_name(host)?;
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()?
            .previews
            .iter()
            .position(|preview| *preview == name)
    }
}

/// The lowercase name in a `Host` value, without port or trailing dot. Addresses and
/// malformed values never equal a published hostname.
fn host_name(host: &str) -> Option<String> {
    let authority = host.parse::<hyper::http::uri::Authority>().ok()?;
    Some(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

/// A caller that came through the Connect tunnel: its connector runs on this machine, so
/// the socket is loopback, and the request names the managed endpoint's hostname.
pub(crate) fn is_relay_reach(hosts: &RelayPreviewHosts, local_ip: IpAddr, host: Option<&str>) -> bool {
    local_ip.to_canonical().is_loopback()
        && host.and_then(host_name).is_some_and(|name| name == hosts.hostname)
}

/// The target each preview hostname serves; a slot whose target closed is free.
#[derive(Default)]
pub(crate) struct RelaySlots(Mutex<[Option<TargetRoute>; RELAY_PREVIEW_SLOTS]>);

impl RelaySlots {
    /// The slot for `route` among the first `available`: the one it holds, else a free one,
    /// else the one whose target is idle with the oldest activity. `None` when every slot's
    /// target has live connections.
    pub(crate) fn assign(&self, route: &TargetRoute, available: usize) -> Option<usize> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let slots = &mut guard[..available.min(RELAY_PREVIEW_SLOTS)];
        let index = slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|held| held.same(route)))
            .or_else(|| slots.iter().position(|slot| slot.as_ref().is_none_or(TargetRoute::is_closed)))
            .or_else(|| {
                slots
                    .iter()
                    .enumerate()
                    .filter_map(|(index, slot)| {
                        slot.as_ref()
                            .filter(|held| held.is_idle())
                            .map(|held| (held.last_activity_ms(), index))
                    })
                    .min()
                    .map(|(_, index)| index)
            })?;
        slots[index] = Some(route.clone());
        Some(index)
    }

    /// The live target in slot `index`.
    pub(crate) fn route(&self, index: usize) -> Option<TargetRoute> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)?
            .clone()
            .filter(|route| !route.is_closed())
    }
}
```

Tests in the same file (`#[cfg(test)] mod tests`): `host_name` handles `ENV-P1.Zone.Test:443` → `env-p1.zone.test`, trailing dot, IPv6 literal, garbage → `None`; `preview_slot` finds index 0–3 and `None` for the endpoint hostname, another environment's name, or nothing published; `is_relay_reach` true only for loopback (`127.0.0.1`, `::1`, `::ffff:127.0.0.1`) plus the endpoint hostname, false for a LAN address or a preview hostname. Slot tests build real targets with a helper:

```rust
    async fn running_target(ctx: &GatewayContext) -> RunningTarget {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        start_target(
            listener,
            GatewayTarget {
                thread_id: "t".into(),
                upstream: "127.0.0.1:9".parse().unwrap(),
                fallback: None,
                upstream_port: 9,
                environment_label: "devbox".into(),
            },
            ctx.clone(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .unwrap()
    }
```

(`ctx` from `super::super::registry::gateway_context(Arc::new(Live), vec![1; 32], "session".into())` with a `Live` principal like `registry.rs` tests.) Cases:
- the same target keeps its slot; a second target gets slot 1;
- with `available = 2` and both slots busy (a held `Activity` via an open TCP connection to the target's port), `assign` returns `None`;
- with both idle, the one with the older `last_activity_ms` is replaced (set via `running.last_activity_ms.store`);
- a target whose `shutdown` is cancelled frees its slot (`route()` returns `None`, the next `assign` reuses it).

- [ ] **Step 3: Run**

Run: `CARGO_TARGET_DIR=$PWD/target systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo test -p bibcode-server --lib preview::gateway::relay`
Expected: PASS after Step 2's code compiles (the tests are written alongside the module; confirm each assertion fails if its branch is removed by temporarily breaking `assign`'s idle filter once, then restore).

---

### Task 5: Server — publish hostnames, restore the tunnel, link-state flags

**Files:**
- Modify: `apps/server/src/production/managed_endpoint.rs`
- Modify: `apps/server/src/production/connect_mcp.rs` (`link_state`, new `restore_managed_endpoint`)
- Modify: `apps/server/src/production/runtime.rs:315-316,457-466` (shared `RelayHosts`)
- Modify: `apps/server/src/lifecycle.rs:~389` (spawn restore after `attach_connect_mcp`)
- Test: `apps/server/src/production/managed_endpoint.rs` tests, `apps/server/tests/cloud_observability_text_generation_domain.rs`, `apps/server/src/production/connect_mcp/tests.rs`, `apps/server/tests/production_connect_mcp.rs`

**Interfaces:**
- Consumes: `RelayHosts`, `RelayPreviewHosts`, `RELAY_PREVIEW_SLOTS` (Task 4).
- Produces: `ManagedEndpointRuntime::with_relay_hosts(self, RelayHosts) -> Self`, `ManagedEndpointRuntime::relay_hosts(&self) -> RelayHosts`; `ConnectMcpService::restore_managed_endpoint(&self) -> Result<Option<Value>, ConnectMcpError>`; link state JSON `managedEndpoint`, `previewHostnames`.

- [ ] **Step 1: Failing tests**

`cloud_observability_text_generation_domain.rs`, extend `managed_endpoint_runtime_reuses_replaces_and_stops_connectors` (it already runs a long-running fixture connector):

```rust
    let hosts = runtime.relay_hosts();
    let with_previews = json!({
        "providerKind":"cloudflare_tunnel",
        "connectorToken":"fixture-token-2",
        "tunnelId":"tunnel-2",
        "tunnelName":"Replacement tunnel",
        "hostname":"env.zone.test",
        "previewHostnames":["env-p1.zone.test","env-p2.zone.test","env-p3.zone.test","env-p4.zone.test","env-p5.zone.test"],
    });
    let republished = runtime.apply(with_previews).await.unwrap();
    // Same connector key: the running connector is kept, only the hostnames change.
    assert_eq!(republished["pid"], replacement["pid"]);
    assert_eq!(
        hosts.current(),
        Some(RelayPreviewHosts {
            hostname: "env.zone.test".into(),
            previews: (1..=4).map(|slot| format!("env-p{slot}.zone.test")).collect(),
        })
    );
    assert!(runtime
        .apply(json!({"providerKind":"cloudflare_tunnel","connectorToken":"fixture-token-2","hostname":"Bad_Host"}))
        .await
        .is_err());
    runtime.apply(serde_json::Value::Null).await.unwrap();
    assert_eq!(hosts.current(), None);
```

(Place it after the `replacement` assertions and before the final stop; import `bibcode_server::preview::gateway::relay::RelayPreviewHosts`.) In the unsupported/missing-connector test, assert `runtime.relay_hosts().current()` is `None` after the "not installed" config even when it carries `hostname`.

`connect_mcp/tests.rs`:

```rust
#[tokio::test]
async fn link_state_reports_the_managed_endpoint_and_its_preview_hostnames() {
    let harness = harness(4, 4).await;
    let state = |harness: &Harness| {
        let service = harness.service.clone();
        async move {
            service
                .json(JsonOperation::ConnectLinkState, None, context(HeaderMap::new()))
                .await
                .expect("link state")
                .body
        }
    };
    let unlinked = state(&harness).await;
    assert_eq!((unlinked["managedEndpoint"].clone(), unlinked["previewHostnames"].clone()), (json!(false), json!(false)));

    link(&harness).await;
    let linked = state(&harness).await;
    assert_eq!((linked["managedEndpoint"].clone(), linked["previewHostnames"].clone()), (json!(true), json!(false)));

    let mut config = relay_config("cloud-user");
    config["endpointRuntime"]["hostname"] = json!("env.zone.test");
    config["endpointRuntime"]["previewHostnames"] = json!(["env-p1.zone.test"]);
    harness
        .service
        .json(JsonOperation::ConnectRelayConfig, Some(config), context(HeaderMap::new()))
        .await
        .expect("relink");
    assert_eq!(state(&harness).await["previewHostnames"], true);
}

#[tokio::test]
async fn restore_reapplies_the_stored_endpoint_only_when_linked() {
    let harness = harness(4, 4).await;
    assert_eq!(harness.service.restore_managed_endpoint().await.unwrap(), None);
    assert!(harness.controls.endpoint_calls.lock().await.is_empty());

    link(&harness).await;
    harness.controls.endpoint_calls.lock().await.clear();
    let restored = harness.service.restore_managed_endpoint().await.unwrap();
    assert_eq!(restored, Some(json!({"status":"running"})));
    assert_eq!(
        *harness.controls.endpoint_calls.lock().await,
        vec![json!({"providerKind":"cloudflare_tunnel","connectorToken":"token"})]
    );

    *harness.controls.endpoint_result.lock().await = Some(Err("offline".into()));
    assert!(harness.service.restore_managed_endpoint().await.is_err());
}
```

`production_connect_mcp.rs`, in `cloud_config_is_persistent_atomic_and_replay_safe`: after `reopened`, call `reopened.restore_managed_endpoint().await.unwrap()` and assert `Some(json!({"status":"running"}))`; for the earlier `link_service` helper that stores `"endpointRuntime": null`, add an assertion in the test that uses it that `restore_managed_endpoint` returns `None`.

- [ ] **Step 2: Run to see failures**

Run: `CARGO_TARGET_DIR=$PWD/target systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo test -p bibcode-server --test cloud_observability_text_generation_domain --test production_connect_mcp && … cargo test -p bibcode-server --lib production::connect_mcp`
Expected: compile errors for the missing methods/fields.

- [ ] **Step 3: Implement `managed_endpoint.rs`**

Add the field `relay_hosts: RelayHosts` to `ManagedEndpointRuntime` (initialized `RelayHosts::default()` in `with_process_attribution`), plus:

```rust
    /// Publishes the running tunnel's hostnames into `hosts` (shared with the preview gateway).
    #[must_use]
    pub fn with_relay_hosts(mut self, hosts: RelayHosts) -> Self {
        self.relay_hosts = hosts;
        self
    }

    #[must_use]
    pub fn relay_hosts(&self) -> RelayHosts {
        self.relay_hosts.clone()
    }
```

Extend the config:

```rust
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedEndpointConfig {
    provider_kind: String,
    connector_token: String,
    tunnel_id: Option<String>,
    tunnel_name: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    preview_hostnames: Vec<String>,
}

impl ManagedEndpointConfig {
    /// The hostnames to publish, `None` without an endpoint hostname (an older relay).
    /// Every name must be a lowercase DNS name; previews beyond [`RELAY_PREVIEW_SLOTS`]
    /// are ignored so a newer relay that provisions more cannot break this server.
    fn relay_hosts(&self) -> Result<Option<RelayPreviewHosts>, String> {
        let Some(hostname) = &self.hostname else {
            return Ok(None);
        };
        if let Some(invalid) = std::iter::once(hostname)
            .chain(&self.preview_hostnames)
            .find(|name| !is_dns_name(name))
        {
            return Err(format!("invalid managed endpoint hostname: {invalid}"));
        }
        Ok(Some(RelayPreviewHosts {
            hostname: hostname.clone(),
            previews: self.preview_hostnames.iter().take(RELAY_PREVIEW_SLOTS).cloned().collect(),
        }))
    }
}

/// A lowercase, multi-label DNS name.
fn is_dns_name(name: &str) -> bool {
    name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
```

In `apply`: publish `None` on the null and unsupported branches; compute `let hosts = config.relay_hosts()?;` right after the empty-token check; in the reuse branch (`active.key == key` and live) set `active.config = config;` then `self.relay_hosts.publish(hosts);` before storing `active` back; publish `None` just before every `terminate_connector(active).await` that replaces a connector and on the "not installed"/spawn-failure `failed_status` returns; publish `hosts` after storing the new `ActiveConnector`. In `shutdown`, publish `None`.

Unit test in `managed_endpoint.rs` `tests`: `is_dns_name` accepts `env-p1.zone.test`, rejects `Env.zone`, `env`, `-a.zone`, `a..zone`, `a_b.zone`.

- [ ] **Step 4: Implement `connect_mcp.rs`**

```rust
    /// Restarts the managed tunnel from the stored link after a server restart. An unlinked
    /// server, or a link without a managed endpoint, has nothing to restore.
    pub async fn restore_managed_endpoint(&self) -> Result<Option<Value>, ConnectMcpError> {
        if self.read_secret("cloud-linked-user-id").await?.is_none() {
            return Ok(None);
        }
        let Some(config) = self.stored_endpoint_runtime().await? else {
            return Ok(None);
        };
        (self.endpoint.0)(config)
            .await
            .map(Some)
            .map_err(|_| ConnectMcpError::internal("Managed endpoint runtime could not be started."))
    }

    /// The stored, non-null managed endpoint runtime config.
    async fn stored_endpoint_runtime(&self) -> Result<Option<Value>, ConnectMcpError> {
        Ok(self
            .read_secret("cloud-endpoint-runtime-config")
            .await?
            .and_then(|stored| serde_json::from_str::<Value>(&stored).ok())
            .filter(|config| !config.is_null()))
    }
```

`link_state` adds:

```rust
        let endpoint = self.stored_endpoint_runtime().await?;
        let preview_hostnames = endpoint
            .as_ref()
            .and_then(|config| config.get("previewHostnames"))
            .and_then(Value::as_array)
            .is_some_and(|names| !names.is_empty());
        // … existing fields, plus:
            "managedEndpoint": endpoint.is_some(),
            "previewHostnames": preview_hostnames,
```

- [ ] **Step 5: Wire `runtime.rs` and `lifecycle.rs`**

`runtime.rs`: before `managed_endpoint`:

```rust
        let relay_hosts = RelayHosts::default();
        let managed_endpoint = ManagedEndpointRuntime::with_process_attribution(process_attribution.clone())
            .with_relay_hosts(relay_hosts.clone());
```

and pass `relay_hosts` as the new last argument of `PreviewGateway::new` (Task 6 adds the parameter). Add `pub fn preview_gateway(&self) -> PreviewGateway { self.preview_gateway.clone() }` to `ProductionRuntime`.

`lifecycle.rs`, right after `runtime.attach_connect_mcp(connect.clone()).await;`:

```rust
                // The tunnel only started on a link; a restart brings it back from the stored one.
                tokio::spawn({
                    let connect = connect.clone();
                    async move {
                        match connect.restore_managed_endpoint().await {
                            Ok(Some(status)) if status["status"] != "running" => tracing::warn!(
                                status = %status["status"],
                                "BiBCode Connect tunnel was not restored"
                            ),
                            Ok(_) => {}
                            Err(error) => tracing::warn!(
                                status = %error.status,
                                "BiBCode Connect tunnel could not be restored"
                            ),
                        }
                    }
                });
```

(`error.status` is the existing public `StatusCode` field; if it is private, use `error.status()` as `production_connect_mcp.rs` does.)

- [ ] **Step 6: Run** the Step 2 commands. Expected: PASS.

---

### Task 6: Server — relay admission, slots, and the `origin` result

**Files:**
- Modify: `apps/server/src/preview/gateway/registry.rs`
- Modify: `apps/server/src/production/workspace_preview.rs:768-810`
- Modify: callers of `PreviewGateway::new`: `apps/server/src/production/runtime.rs`, `apps/server/tests/preview_gateway_admission.rs:57`, `apps/server/tests/preview_open_url.rs:189`, the four calls in `registry.rs` tests
- Test: `apps/server/src/preview/gateway/registry.rs` tests

**Interfaces:**
- Consumes: Tasks 4–5.
- Produces: `PreviewGateway::new(bind_host, environment_label, ctx, preview, relay_hosts: RelayHosts)`; `GatewayOpenResult.origin: Option<String>`; `GatewayError::{RelayNotProvisioned, RelayBusy}`; wire reasons `relay-not-provisioned`, `relay-busy`; wire field `origin`.

- [ ] **Step 1: Failing tests (registry.rs `tests`)**

```rust
    fn relay_gateway(hosts: Option<RelayPreviewHosts>) -> PreviewGateway {
        let relay = RelayHosts::default();
        relay.publish(hosts);
        PreviewGateway::new(
            "127.0.0.1",
            "devbox",
            gateway_context(Arc::new(Live), vec![1; 32], "session".into()),
            PreviewManager::new(),
            relay,
        )
    }

    fn published(previews: usize) -> Option<RelayPreviewHosts> {
        Some(RelayPreviewHosts {
            hostname: "env.zone.test".into(),
            previews: (1..=previews).map(|slot| format!("env-p{slot}.zone.test")).collect(),
        })
    }

    #[tokio::test]
    async fn a_relay_caller_gets_a_preview_hostname_origin() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://localhost:{}/", upstream.local_addr().unwrap().port());
        let gateway = relay_gateway(published(4));
        let relay = reach("127.0.0.1", Some("env.zone.test"));
        let first = gateway.open("t", &url, "s", i64::MAX, Some(&relay)).await.unwrap();
        assert_eq!(first.origin.as_deref(), Some("https://env-p1.zone.test"));
        let again = gateway.open("t", &url, "s", i64::MAX, Some(&relay)).await.unwrap();
        assert_eq!(again.origin, first.origin, "a target keeps its slot");
        // An SSH client on the same thread and port shares the target but gets no origin.
        let ssh = gateway
            .open("t", &url, "s", i64::MAX, Some(&reach("127.0.0.1", Some("127.0.0.1:45123"))))
            .await
            .unwrap();
        assert_eq!((ssh.gateway_port, ssh.origin), (first.gateway_port, None));
        gateway.shutdown().await;
    }

    #[tokio::test]
    async fn relay_admission_needs_published_preview_hostnames() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://localhost:{}/", upstream.local_addr().unwrap().port());
        let relay = reach("127.0.0.1", Some("env.zone.test:443"));
        assert!(matches!(
            relay_gateway(published(0)).open("t", &url, "s", i64::MAX, Some(&relay)).await,
            Err(GatewayError::RelayNotProvisioned)
        ));
        // Without published hosts, the relay `Host` is just a proxied name.
        assert!(matches!(
            relay_gateway(None).open("t", &url, "s", i64::MAX, Some(&relay)).await,
            Err(GatewayError::NotReachable(Unreachable::Proxied))
        ));
    }

    #[tokio::test]
    async fn relay_opens_fail_busy_when_every_slot_is_live() {
        let gateway = relay_gateway(published(1));
        let relay = reach("127.0.0.1", Some("env.zone.test"));
        let upstreams: Vec<_> = (0..2).map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap()).collect();
        let url = |index: usize| format!("http://localhost:{}/", upstreams[index].local_addr().unwrap().port());
        let first = gateway.open("t", &url(0), "s", i64::MAX, Some(&relay)).await.unwrap();
        // A held client connection keeps the first target live.
        let _held = TcpStream::connect(("127.0.0.1", first.gateway_port)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            gateway.open("t", &url(1), "s", i64::MAX, Some(&relay)).await,
            Err(GatewayError::RelayBusy)
        ));
        gateway.shutdown().await;
    }
```

- [ ] **Step 2: Run to see failures**

Run: `… cargo test -p bibcode-server --lib preview::gateway::registry`
Expected: compile errors (new constructor argument, variants, field).

- [ ] **Step 3: Implement**

`Inner` gains `relay_hosts: RelayHosts` and `relay_slots: RelaySlots`; `new` takes `relay_hosts: RelayHosts` last and stores `relay_slots: RelaySlots::default()`. `GatewayOpenResult` gains `pub origin: Option<String>` (`mint` sets `None`). `GatewayError` gains:

```rust
    #[error("Previews over BiBCode Connect aren't set up for this environment yet.")]
    RelayNotProvisioned,
    #[error("Every BiBCode Connect preview address of this environment is in use.")]
    RelayBusy,
```

In `open`, replace the `bind_host` line:

```rust
        let relay_previews = self.relay_previews(reach)?;
        let bind_host = match (&relay_previews, reach) {
            // The tunnel connector reaches the target's handler through the main port.
            (Some(_), Some(reach)) => reach.local_ip.to_canonical().to_string(),
            _ => gateway_bind_host(reach, &self.inner.bind_host)?,
        };
```

and replace the final `Ok(self.mint(...))`:

```rust
        let origin = match relay_previews {
            Some(previews) => {
                let Some(slot) = self
                    .inner
                    .relay_slots
                    .assign(&target.running.route, previews.len())
                else {
                    tracing::warn!(upstream_port, "every BiBCode Connect preview hostname is in use");
                    return Err(GatewayError::RelayBusy);
                };
                Some(format!("https://{}", previews[slot]))
            }
            None => None,
        };
        let mut opened = self.mint(&target.running, &key, session_id, session_expiry);
        opened.origin = origin;
        Ok(opened)
```

New helper on `PreviewGateway`:

```rust
    /// The preview hostnames for a caller that came through the Connect tunnel; `None` for
    /// every other caller.
    fn relay_previews(&self, reach: Option<&ClientReach>) -> Result<Option<Vec<String>>, GatewayError> {
        let (Some(reach), Some(hosts)) = (reach, self.inner.relay_hosts.current()) else {
            return Ok(None);
        };
        if !relay::is_relay_reach(&hosts, reach.local_ip, reach.host.as_deref()) {
            return Ok(None);
        }
        if hosts.previews.is_empty() {
            return Err(GatewayError::RelayNotProvisioned);
        }
        Ok(Some(hosts.previews))
    }
```

`workspace_preview.rs`: `gateway_error` maps `RelayNotProvisioned => "relay-not-provisioned"`, `RelayBusy => "relay-busy"`; `gateway_open` builds the JSON with `"origin"` only when `Some`:

```rust
    let mut result = json!({
        "gatewayPort": opened.gateway_port,
        "capability": opened.capability,
        "expiresAtMs": opened.expires_at_ms,
    });
    if let Some(origin) = opened.origin {
        result["origin"] = json!(origin);
    }
    Ok(result)
```

Update every `PreviewGateway::new` caller with `RelayHosts::default()` (tests) or the shared `relay_hosts` (runtime).

- [ ] **Step 4: Run**

Run: `… cargo test -p bibcode-server --lib preview::gateway && … cargo test -p bibcode-server --test preview_gateway_admission --test preview_open_url --test production_workspace_preview_rpc`
Expected: PASS.

---

### Task 7: Server — HTTPS-facing routed handling and the main-port router

**Files:**
- Modify: `apps/server/src/preview/gateway/proxy.rs` (generic body, `https` flag, `Secure`, `serve_https`, `expired_response`)
- Modify: `apps/server/src/preview/gateway/registry.rs` (`route_relay`)
- Modify: `apps/server/src/http.rs` (`AppState.preview_gateway`, outermost middleware)
- Modify: `apps/server/src/lifecycle.rs:424-433` (pass the gateway into `AppState`)
- Test: `apps/server/src/preview/gateway/relay.rs` tests (routed exchanges), `apps/server/src/http.rs` tests (middleware pass-through)

**Interfaces:**
- Consumes: Tasks 4–6.
- Produces: `TargetRoute::serve_https<B: GatewayBody>(&self, Request<B>) -> Response<ProxyBody>`; `proxy::expired_response() -> Response<ProxyBody>`; `PreviewGateway::route_relay<B: GatewayBody>(&self, local_ip: Option<IpAddr>, Request<B>) -> Result<Response<ProxyBody>, Request<B>>`; `pub trait GatewayBody`.

- [ ] **Step 1: Failing tests in `relay.rs`**

A test fixture serves `route_relay` on a real socket so upgrades work:

```rust
    /// Serves `gateway.route_relay` on loopback as the main port would, answering 404 for
    /// requests it hands back.
    async fn front(gateway: PreviewGateway) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let gateway = gateway.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let gateway = gateway.clone();
                        async move {
                            Ok::<_, Infallible>(
                                match gateway.route_relay(Some(Ipv4Addr::LOCALHOST.into()), request).await {
                                    Ok(response) => response,
                                    Err(_) => Response::builder().status(404).body(empty()).unwrap(),
                                },
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
        });
        addr
    }
```

The upstream is an axum app with `GET /` → redirect to `http://localhost:<port>/next`, `POST /echo` → `"posted"`, and `GET /ws` (axum `WebSocketUpgrade`) that rejects unless `Origin == "http://" + Host` and echoes text frames. Tests (reqwest with `.no_proxy()`, redirects off, sending `Host: env-p1.zone.test`; tungstenite with a request whose `Host`/`Origin` headers are set):

1. **bootstrap is Secure**: `gateway.open` with relay reach → capability; `GET /__bibcode/bootstrap?cap=…&to=%2F` with `Host: env-p1.zone.test` → `200`, `Set-Cookie` contains `; Secure;` and `HttpOnly`.
2. **location rewritten to https**: with that cookie, `GET /` → `Location: https://env-p1.zone.test/next`.
3. **https Origin rule**: `POST /echo` with `Origin: http://env-p1.zone.test` → `403`; with `Origin: https://env-p1.zone.test` → `200 "posted"`.
4. **WebSocket through the router**: upgrade `/ws` with cookie and `Origin: https://env-p1.zone.test` → echo works (the upstream's Origin==Host check passes after the rewrite).
5. **empty slot**: `Host: env-p2.zone.test` (no target) → `401` with the expired copy.
6. **not a preview host**: `Host: env.zone.test` and `Host: other-p1.zone.test` → `404` (handed back); a request with `local_ip` `192.168.1.5` for `env-p1.zone.test` → handed back (call `route_relay` directly and assert `Err`).
7. **takeover**: with 1 published slot, open target A (idle), bootstrap a cookie for A, open target B on another upstream (takes slot 1); A's cookie on `env-p1.zone.test` → `401`.

- [ ] **Step 2: Run to see failures**

Run: `… cargo test -p bibcode-server --lib preview::gateway::relay`
Expected: compile errors (`route_relay`, `serve_https` missing).

- [ ] **Step 3: Make the proxy path generic and HTTPS-aware (`proxy.rs`)**

```rust
/// A request body the gateway forwards upstream: hyper's own, or axum's for requests the
/// main port routes to a target.
pub trait GatewayBody:
    Body<Data = Bytes, Error: Into<Box<dyn std::error::Error + Send + Sync>>> + Send + 'static
{
}

impl<B> GatewayBody for B where
    B: Body<Data = Bytes, Error: Into<Box<dyn std::error::Error + Send + Sync>>> + Send + 'static
{
}
```

- `handle`, `framing_ui_origin`, `handle_request`, `forward` take `request: Request<B>` with `B: GatewayBody` (the `&Request<B>` borrow in `framing_ui_origin` likewise); `handle` and `handle_request` gain `https: bool`; `handle_request` computes `client_origin(if https { "https" } else { "http" }, &host)` and passes `https` to `bootstrap`.
- `bootstrap(state, query, ui_origin, https)` sets the cookie:

```rust
    let cookie = format!(
        "{}={id}; HttpOnly;{} SameSite=Lax; Path=/",
        gateway_cookie_name(state.gateway_port),
        if https { " Secure;" } else { "" },
    );
```

- `serve_connection` calls `handle(state, request, false)`.
- `forward`: `hyper::client::conn::http1::handshake::<_, B>(TokioIo::new(stream))`.
- Add:

```rust
impl TargetRoute {
    /// Serves a request that reached the main port under this target's BiBCode Connect
    /// preview hostname. TLS ended at Cloudflare, so the client-facing origin is `https`.
    pub async fn serve_https<B: GatewayBody>(&self, request: Request<B>) -> Response<ProxyBody> {
        let _activity = self.0.activity();
        handle(self.0.clone(), request, true).await
    }
}

/// The gateway's expired-link page, for a preview hostname with no target.
#[must_use]
pub fn expired_response() -> Response<ProxyBody> {
    expired()
}
```

- [ ] **Step 4: `route_relay` in `registry.rs`**

```rust
    /// Serves a request that reached the main port on a loopback socket under a published
    /// preview hostname: the slot's target handles it, and an empty slot answers the
    /// expired-link page. Any other request is handed back untouched.
    pub async fn route_relay<B: GatewayBody>(
        &self,
        local_ip: Option<IpAddr>,
        request: Request<B>,
    ) -> Result<Response<ProxyBody>, Request<B>> {
        let slot = local_ip
            .filter(|ip| ip.to_canonical().is_loopback())
            .and_then(|_| request.headers().get(hyper::header::HOST)?.to_str().ok())
            .and_then(|host| self.inner.relay_hosts.preview_slot(host));
        let Some(slot) = slot else {
            return Err(request);
        };
        Ok(match self.inner.relay_slots.route(slot) {
            Some(route) => route.serve_https(request).await,
            None => proxy::expired_response(),
        })
    }
```

- [ ] **Step 5: Outermost middleware in `http.rs`**

`AppState` gains `pub preview_gateway: Option<PreviewGateway>` (lifecycle passes `production_runtime.as_ref().map(|runtime| runtime.preview_gateway())`; other `AppState` constructions in tests pass `None`). In `build_router`, add as the **last** layer (outermost, before CORS and admission):

```rust
        .layer(middleware::from_fn_with_state(
            state.preview_gateway.clone(),
            route_relay_previews,
        ))
```

```rust
/// BiBCode Connect preview hostnames reach this port through the tunnel; they are served
/// by the preview gateway and never by BiBCode's own routes, CORS, or admission gate.
async fn route_relay_previews(
    State(gateway): State<Option<PreviewGateway>>,
    connection: ConnectionInfo,
    request: Request,
    next: Next,
) -> Response {
    let Some(gateway) = gateway else {
        return next.run(request).await;
    };
    let local = connection.ok().and_then(|ConnectInfo(addrs)| addrs.local).map(|addr| addr.ip());
    match gateway.route_relay(local, request).await {
        Ok(response) => response.map(axum::body::Body::new),
        Err(request) => next.run(request).await,
    }
}
```

`http.rs` test: a `Router::new().route("/api", get(|| async { "api" }))` with this layer and a gateway whose `RelayHosts` publishes `env-p1.zone.test`, served with `into_make_service_with_connect_info::<ConnectionAddrs>()`; `Host: env-p1.zone.test` → `401` gateway page (no target), `Host: localhost` → `"api"`.

- [ ] **Step 6: Run**

Run: `… cargo test -p bibcode-server --lib preview::gateway && … cargo test -p bibcode-server --lib http && … cargo test -p bibcode-server --test preview_gateway --test preview_gateway_admission`
Expected: PASS.

---

### Task 8: Web — relay resolution

**Files:**
- Modify: `apps/web/src/browser/browserTargetResolver.ts`
- Modify: `apps/web/src/browser/previewGateway.ts`
- Test: `apps/web/src/browser/browserTargetResolver.test.ts`, `apps/web/src/browser/previewGateway.test.ts`

**Interfaces:**
- Consumes: `PreviewGatewayOpenResult.origin`, reasons (Task 1).
- Produces: `PreviewTargetResolution` variant `{ kind: "gateway"; url: string; via: "relay" }`; `PreviewUnreachableReason` without `"relay"`.

- [ ] **Step 1: Failing tests**

`browserTargetResolver.test.ts`: replace "refuses relay environments" with

```ts
  it("routes relay loopback through the gateway on canonical localhost", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "RelayConnectionTarget" }, "https://abc.connect.example.com"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://127.0.0.1:8080/x")).toEqual({
      kind: "gateway",
      via: "relay",
      url: "http://localhost:8080/x",
    });
  });
```

and loop `["ssh", "public-host"]` in "names the environment without transport jargon".

`previewGateway.test.ts`:

```ts
const relay = () =>
  connection({ _tag: "RelayConnectionTarget", connectionId: "r:1" }, "https://env.zone.test/");
const relayOpened = (origin?: string): GatewayOpenResult =>
  AsyncResult.success({ gatewayPort: 41000, capability: "CAP", expiresAtMs: 0, ...(origin ? { origin } : {}) });

describe("relay", () => {
  it("bootstraps on the preview hostname and maps it back to localhost", async () => {
    readPreparedConnection.mockReturnValue(relay());
    const result = await resolveForNavigation({
      environmentId, threadId, tabId: "tab-1",
      canonicalUrl: "http://localhost:5173/app?x=1",
      gatewayOpen: vi.fn(async () => relayOpened("https://env-p2.zone.test")),
    });
    expect(result).toEqual({
      kind: "ok",
      url: "https://env-p2.zone.test/__bibcode/bootstrap?cap=CAP&to=%2Fapp%3Fx%3D1",
    });
    expect(canonicalizePreviewUrl("https://env-p2.zone.test/next")).toBe("http://localhost:5173/next");
    expect(sshForward).not.toHaveBeenCalled();
  });

  it("says previews aren't set up when the server returns no origin or refuses", async () => {
    readPreparedConnection.mockReturnValue(relay());
    const message =
      "Previews over BiBCode Connect aren't set up for Build box yet. Open BiBCode on Build box to finish setting them up.";
    for (const gatewayOpen of [
      vi.fn(async () => relayOpened()),
      vi.fn(async () => refused("relay-not-provisioned")),
      vi.fn(async () => refused("not-reachable")),
    ]) {
      await expect(
        resolveForNavigation({ environmentId, threadId, canonicalUrl: "http://localhost:5173/", gatewayOpen }),
      ).resolves.toEqual({ kind: "unreachable", message });
    }
  });

  it("asks to close a preview when every preview hostname is busy", async () => {
    readPreparedConnection.mockReturnValue(relay());
    await expect(
      resolveForNavigation({
        environmentId, threadId, canonicalUrl: "http://localhost:5173/",
        gatewayOpen: vi.fn(async () => refused("relay-busy")),
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message: "Too many previews of Build box are open over BiBCode Connect. Close one, then try again.",
      retryable: true,
    });
  });

  it("never frames a relay preview in browser mode, but opens it in a new tab", async () => {
    vi.stubGlobal("window", {});
    vi.stubGlobal("location", { origin: "http://192.168.1.5:3773", protocol: "http:" });
    readPreparedConnection.mockReturnValue(relay());
    const gatewayOpen = vi.fn(async () => relayOpened("https://env-p1.zone.test"));
    await expect(
      resolveForNavigation({ environmentId, threadId, tabId: "tab-1", canonicalUrl: "http://localhost:5173/", gatewayOpen }),
    ).resolves.toEqual({
      kind: "unreachable",
      message:
        "Build box is reached through BiBCode Connect, so its previews can't show inside this page. Click the address in the chat or terminal to open it in a new tab.",
    });
    expect(gatewayOpen).not.toHaveBeenCalled();
    await expect(
      resolveForNavigation({ environmentId, threadId, canonicalUrl: "http://localhost:5173/", gatewayOpen }),
    ).resolves.toEqual({ kind: "ok", url: "https://env-p1.zone.test/__bibcode/bootstrap?cap=CAP&to=%2F" });
  });
});
```

- [ ] **Step 2: Run to see failures**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/browser/browserTargetResolver.test.ts src/browser/previewGateway.test.ts`
Expected: the new and changed tests FAIL.

- [ ] **Step 3: Implement `browserTargetResolver.ts`**

- `PreviewUnreachableReason = "disconnected" | "ssh" | "public-host"`; drop `relay` from `UNREACHABLE_MESSAGES`.
- `EnvironmentReach` gains `| { readonly kind: "relay" }`; `classifyEnvironmentReach` returns `{ kind: "relay" }` for `RelayConnectionTarget`.
- `PreviewTargetResolution` gains `| { readonly kind: "gateway"; readonly url: string; readonly via: "relay" }` and the doc comment notes "over BiBCode Connect the server names the preview hostname to bootstrap on".
- The final return becomes:

```ts
  return reach.kind === "host"
    ? { kind: "gateway", via: "host", host: reach.host, url }
    : { kind: "gateway", via: reach.kind, url };
```

- [ ] **Step 4: Implement `previewGateway.ts`**

- `GatewayOpenMutation`'s success type gains `origin?: string`.
- Messages:

```ts
const relayNotReadyMessage = (label: string) =>
  `Previews over BiBCode Connect aren't set up for ${label} yet. Open BiBCode on ${label} to finish setting them up.`;
const relayFrameMessage = (label: string) =>
  `${label} is reached through BiBCode Connect, so its previews can't show inside this page. Click the address in the chat or terminal to open it in a new tab.`;
```

- After the SSH browser-mode check:

```ts
  // Another site's BiBCode page never sends the preview's Lax cookie inside a frame;
  // a new tab (no tab id) loads it fine.
  if (resolution.via === "relay" && isBrowserMode() && tabId !== undefined) {
    return { kind: "unreachable", message: relayFrameMessage(label) };
  }
```

- `gatewayFailure(result, label, canonical, relay: boolean)` (call site passes `resolution.via === "relay"`), with new cases before `not-reachable`:

```ts
    case "relay-not-provisioned":
      return { message: relayNotReadyMessage(label) };
    case "relay-busy":
      return {
        message: `Too many previews of ${label} are open over BiBCode Connect. Close one, then try again.`,
        retryable: true,
      };
    case "not-reachable":
      // A relay caller is refused this way only by a link that predates preview hostnames.
      if (relay) return { message: relayNotReadyMessage(label) };
      return { message: `${error.message} Open it on ${label} directly.` };
```

- Client origin selection:

```ts
  const { gatewayPort, capability, origin } = result.value;

  let clientOrigin: string;
  if (resolution.via === "relay") {
    if (origin === undefined) return fail({ message: relayNotReadyMessage(label) });
    clientOrigin = origin;
    canonicalOrigins.set(clientOrigin, canonical.origin);
  } else if (resolution.via === "host") {
    // … unchanged
  } else {
    // … unchanged SSH branch
  }
```

- `ui=` only for frameable gateways: `const ui = isBrowserMode() && resolution.via !== "relay" ? … : "";`

- [ ] **Step 5: Run** the Step 2 command plus `src/browser/openLink.test.ts`. Expected: PASS.

---

### Task 9: Web — automatic re-link for preview hostnames

**Files:**
- Modify: `apps/web/src/cloud/linkEnvironment.ts` (`installRelayClient` option)
- Modify: `apps/web/src/cloud/linkEnvironmentAtoms.ts` (pass the option through)
- Create: `apps/web/src/cloud/previewHostRelink.tsx`
- Modify: `apps/web/src/AppRoot.tsx` (mount `<PreviewHostRelink />` with the other global coordinators)
- Test: `apps/web/src/cloud/previewHostRelink.test.ts`, `apps/web/src/cloud/linkEnvironment.test.ts`

**Interfaces:**
- Consumes: `EnvironmentCloudLinkStateResult.{managedEndpoint, previewHostnames}` (Task 1); `readManagedRelayClerkToken()` (`managedAuth.tsx`); `usePrimaryCloudLinkState()`.
- Produces: `needsPreviewHostRelink(state: CloudLinkState | null): boolean`; `relinkForPreviewHosts(deps): Promise<void>`; `PreviewHostRelink(): null`.

- [ ] **Step 1: Failing tests**

`previewHostRelink.test.ts`:

```ts
import { describe, expect, it, vi } from "vite-plus/test";

import { needsPreviewHostRelink, relinkForPreviewHosts } from "./previewHostRelink";

const state = (overrides: Record<string, unknown>) => ({
  linked: true, cloudUserId: "u", relayUrl: "https://relay.test/", relayIssuer: null,
  managedEndpoint: true, previewHostnames: false, ...overrides,
});
const target = { environmentId: "env-1", label: "Desktop", httpBaseUrl: "http://127.0.0.1:3000", wsBaseUrl: "ws://127.0.0.1:3000" };

describe("needsPreviewHostRelink", () => {
  it("is true only for a linked managed tunnel without preview hostnames", () => {
    expect(needsPreviewHostRelink(state({}))).toBe(true);
    expect(needsPreviewHostRelink(state({ previewHostnames: true }))).toBe(false);
    expect(needsPreviewHostRelink(state({ managedEndpoint: false }))).toBe(false);
    expect(needsPreviewHostRelink(state({ linked: false }))).toBe(false);
    // A server that predates the flags reports neither.
    expect(needsPreviewHostRelink(state({ managedEndpoint: undefined, previewHostnames: undefined }))).toBe(false);
    expect(needsPreviewHostRelink(null)).toBe(false);
  });
});

describe("relinkForPreviewHosts", () => {
  it("re-links once per environment per session, silently, and refreshes the state", async () => {
    const attempted = new Set<string>();
    const link = vi.fn(async () => ({ _tag: "Success" as const }));
    const refresh = vi.fn();
    const deps = { target, readToken: async () => "clerk", link, refresh, attempted };
    await relinkForPreviewHosts(deps);
    await relinkForPreviewHosts(deps);
    expect(link).toHaveBeenCalledTimes(1);
    expect(link).toHaveBeenCalledWith({ target, clerkToken: "clerk", installRelayClient: false });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("waits for a sign-in instead of spending the session's attempt", async () => {
    const attempted = new Set<string>();
    const link = vi.fn(async () => ({ _tag: "Success" as const }));
    await relinkForPreviewHosts({ target, readToken: async () => null, link, refresh: vi.fn(), attempted });
    expect(link).not.toHaveBeenCalled();
    expect(attempted.size).toBe(0);
  });
});
```

`linkEnvironment.test.ts`: following the existing relay-client status mocks in that file, add a case where `cloudGetRelayClientStatus` reports `{ status: "missing", … }` and `linkPrimaryEnvironmentToCloud({ target: TARGET, clerkToken: "t", installRelayClient: false })` fails with `"The relay client is not installed."` and the install-confirmation request is never made.

- [ ] **Step 2: Run to see failures**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/cloud/previewHostRelink.test.ts src/cloud/linkEnvironment.test.ts`
Expected: FAIL (module missing; option unsupported).

- [ ] **Step 3: Implement**

`linkEnvironment.ts`: `ensureRelayClientAvailable(environmentId, installIfMissing: boolean)`; after the `unsupported` check:

```ts
    if (!installIfMissing) {
      return yield* new CloudEnvironmentLinkError({ message: "The relay client is not installed." });
    }
```

`linkPrimaryEnvironmentToCloud(input: { target; clerkToken; installRelayClient?: boolean })` passes `input.installRelayClient ?? true`. `linkEnvironmentAtoms.ts`' `execute` input type gains `readonly installRelayClient?: boolean`.

`previewHostRelink.tsx`:

```tsx
import type { AtomCommandResult } from "@bibcode/client-runtime/state/runtime";
import { useEffect } from "react";

import { useAtomCommand } from "../state/use-atom-command";
import type { CloudLinkState, CloudLinkTarget } from "./linkEnvironment";
import { linkPrimaryEnvironment } from "./linkEnvironmentAtoms";
import { readManagedRelayClerkToken } from "./managedAuth";
import { usePrimaryCloudLinkState } from "./primaryCloudLinkState";

/** A linked managed tunnel whose server has no preview hostnames: linked before they existed. */
export function needsPreviewHostRelink(state: CloudLinkState | null): boolean {
  return state?.linked === true && state.managedEndpoint === true && state.previewHostnames === false;
}

/** Environments re-linked this app session. */
const attemptedThisSession = new Set<string>();

export async function relinkForPreviewHosts(deps: {
  readonly target: CloudLinkTarget;
  readonly readToken: () => Promise<string | null>;
  readonly link: (input: {
    readonly target: CloudLinkTarget;
    readonly clerkToken: string;
    readonly installRelayClient: false;
  }) => Promise<AtomCommandResult<void, unknown> | { readonly _tag: "Success" }>;
  readonly refresh: () => void;
  readonly attempted?: Set<string>;
}): Promise<void> {
  const attempted = deps.attempted ?? attemptedThisSession;
  if (attempted.has(deps.target.environmentId)) return;
  const clerkToken = await deps.readToken().catch(() => null);
  // Signed out: try again once the user signs in.
  if (!clerkToken) return;
  attempted.add(deps.target.environmentId);
  const result = await deps.link({ target: deps.target, clerkToken, installRelayClient: false });
  if (result._tag !== "Success") {
    console.warn("[bibcode-connect] Could not set up BiBCode Connect previews; retrying next session.");
    return;
  }
  deps.refresh();
}

/**
 * Re-links the primary environment once per session when its BiBCode Connect tunnel has no
 * preview hostnames, so environments linked before them gain previews without user action.
 */
export function PreviewHostRelink(): null {
  const { data, target, refresh } = usePrimaryCloudLinkState();
  const link = useAtomCommand(linkPrimaryEnvironment, { reportFailure: false });
  useEffect(() => {
    if (!target || !needsPreviewHostRelink(data)) return;
    void relinkForPreviewHosts({ target, readToken: readManagedRelayClerkToken, link, refresh });
  }, [data, link, refresh, target]);
  return null;
}
```

(Match `link`'s parameter type to what `useAtomCommand` returns; if it differs, type `deps.link` as `(input) => Promise<{ readonly _tag: string }>`.)

`AppRoot.tsx`: render `<PreviewHostRelink />` next to the other global coordinators (`ShareExposureReconciler`, `PreviewAutomationHosts`).

- [ ] **Step 4: Run** the Step 2 command. Expected: PASS.

- [ ] **Step 5: React review** — review `previewHostRelink.tsx` against `/vercel-react-best-practices` (effect deps, no render work, stable callbacks) and record the result for the final report.

---

### Task 10: Documentation and runbooks

**Files:**
- Modify: `docs/architecture/remote.md` (Preview gateway: replace "Relay limitation" with a "Gateway over BiBCode Connect" bullet; BiBCode Connect: tunnel restore on restart; E2EE and cookie-tossing notes)
- Modify: `docs/user/workspace-ui.md` (relay previews; browser-mode new tab)
- Modify: `infra/relay/README.md` (preview hostnames, 5 DNS records per environment)
- Modify: `docs/testing/cross-platform-validation.md` (relay step)
- Modify: `docs/superpowers/specs/2026-10-07-internal-browser-preview-gateway-design.md` (Out of scope: pointer to the relay spec)
- Modify: `CHANGELOG.md` (Unreleased entry) if the repository keeps an Unreleased section

- [ ] **Step 1: `remote.md`** — Under "Preview gateway", add after "Client side":

```markdown
- **Gateway over BiBCode Connect.** Each relay environment has 4 preview
  hostnames, `<env label>-p1` … `-p4` in the tunnel zone, provisioned with the
  environment (CNAMEs to its tunnel, ingress to the server's loopback origin)
  and returned in the tunnel's runtime config. The server publishes them while
  the tunnel runs. A `gatewayOpen` caller on a loopback socket under the
  environment's own hostname is a relay caller: its target takes a slot (its
  own, a free one, or the idle one with the oldest activity; with every slot
  live the call fails `relay-busy`), and the result carries
  `origin: https://<preview hostname>`. Without published preview hostnames the
  call fails `relay-not-provisioned`. On the main port, an outermost layer hands
  every request whose `Host` is a published preview hostname (on a loopback
  socket) to that slot's target, ahead of BiBCode's routes, CORS, and admission
  gate; an empty slot answers the expired-link page. These requests are
  HTTPS-facing: the Origin rule compares against `https://<Host>`, the cookie
  adds `Secure`, and `Location` is rewritten to https. Relay connections carry
  no E2EE, so preview traffic is visible to Cloudflare like all relay traffic. A
  previewed page's script can set cookies for the whole tunnel zone (the gateway
  drops `Domain=` only from upstream `Set-Cookie`); adding the zone to the
  Public Suffix List is a follow-up. Browser mode never frames these previews:
  links open in a new tab, and a navigation inside the panel explains why.
```

Delete the "Relay limitation" bullet. Under "BiBCode Connect" add: "The server restarts the managed tunnel from the stored link config at startup; a primary client whose server's link has no preview hostnames re-links once per session (without the relay-client install prompt) to provision them."

- [ ] **Step 2: user doc, relay README, runbook step, spec pointer** — `workspace-ui.md`: a short "Previews over BiBCode Connect" paragraph (canonical `localhost` URL in the address bar; up to 4 previews at once; browser mode opens them in a new tab). Relay README: preview hostnames and their DNS cost. `cross-platform-validation.md`: a "Relay preview" step — on a deployed relay stage, from a relay-connected desktop and a browser-mode client, open `http://localhost:<vite port>/` from a thread; confirm the address bar shows `localhost`, HMR updates, Reload re-bootstraps, closing the tab and reopening works, and a fifth concurrent preview with live connections shows the busy notice. Phase 1 spec "Out of scope": "Relay reach: see [2026-10-11-preview-gateway-relay-design.md](2026-10-11-preview-gateway-relay-design.md)."

- [ ] **Step 3: Profiler docs check** — no new profiler event or finding: confirm `docs/skills/bibcode-profiler/references/` needs no change and say so in the final report.

---

### Task 11: Gates, review, and the single commit

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all --check` (then `cargo fmt --all` if it fails, and re-run).
Run: `CARGO_TARGET_DIR=$PWD/target systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo clippy -p bibcode-server --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 2: Rust tests**

Run: `… cargo test -p bibcode-server` (detached with `setsid nohup` and a log, per local quirks).
Expected: PASS.

- [ ] **Step 3: TypeScript gates**

Run: `node scripts/run-local-vp.mjs check`, `node scripts/run-local-vp.mjs run typecheck`, `node scripts/run-local-vp.mjs run check:contracts`, `cd infra/relay && node ../../scripts/run-local-vp.mjs test run`, `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/browser src/cloud`.
Expected: all pass.

- [ ] **Step 4: Reviews** — `UI.md` review of the three new messages and the browser-mode behavior; `vercel-react-best-practices` result from Task 9; `git diff` and `git status --short` for stray files (no `.codegraph/`, no `target/`).

- [ ] **Step 5: Codex review** — `codex review -c sandbox_mode="read-only" --uncommitted` (detached, logged); evaluate every finding with `superpowers:receiving-code-review`; fix what holds; re-run affected tests.

- [ ] **Step 6: Commit (with the user's go-ahead for the whole batch)**

```bash
git add -A docs infra/relay packages/contracts apps/server apps/web
git commit -F <message file>
```

Message: `feat(preview): reach the preview gateway over BiBCode Connect` with a body summarizing relay slots, the main-port router, tunnel restore, and the auto re-link, ending with the `Co-Authored-By` line from the session's attribution reminder.
