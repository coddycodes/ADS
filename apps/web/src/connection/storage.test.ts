import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
  BearerConnectionTarget,
  ConnectionStorageChangedError,
  ConnectionTransientError,
  CredentialStore,
  decideStorageIdentity,
  PrimaryConnectionTarget,
  ProfileStore,
  type PreparedConnection,
  verifyPreparedStorageIdentity,
} from "@coddy/client-runtime/connection";
import {
  AcceptedStorageIdentityStore,
  ConnectionCatalogDocument,
  ConnectionCatalogHealthStore,
  type ConnectionCatalogDocument as ConnectionCatalogDocumentType,
  ConnectionRegistrationStore,
  ConnectionTargetStore,
  EnvironmentCacheStore,
} from "@coddy/client-runtime/platform";
import { TokenStore } from "@coddy/client-runtime/authorization";
import {
  EnvironmentId,
  type OrchestrationShellSnapshot,
  type OrchestrationThread,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
} from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { IDBFactory, IDBKeyRange, IDBObjectStore } from "fake-indexeddb";
import { afterEach, vi } from "vite-plus/test";

import {
  type CatalogBackend,
  connectionStorageLayer,
  connectionCatalogLivesInIndexedDb,
  makeCatalogBackend,
  makeCatalogStore,
} from "./storage";

const emptyCatalog = {
  schemaVersion: 1,
  targets: [],
  profiles: [],
  credentials: [],
  remoteDpopTokens: [],
  acceptedStorageIdentities: [],
} as const;
const decodeCatalog = Schema.decodeUnknownSync(Schema.fromJsonString(ConnectionCatalogDocument));
const encodeCatalog = Schema.encodeSync(Schema.fromJsonString(ConnectionCatalogDocument));
const encodeUnknownJson = Schema.encodeUnknownSync(Schema.fromJsonString(Schema.Unknown));
const unusedCatalogCompare: CatalogBackend["compare"] = () =>
  Effect.die(new Error("Catalog comparison is not used by this test."));

// ── In-memory IndexedDB fake ─────────────────────────────────────────
// The production storage code attaches listeners *then* triggers the op, so
// the fake must fire events after the synchronous body has run — every
// request/transaction event is deferred through `queueMicrotask`.

type FaultMode = "none" | "get" | "put" | "delete" | "cursor";

class FakeRequest {
  result: unknown = undefined;
  error: unknown = null;
  private readonly listeners = new Map<string, Array<() => void>>();
  addEventListener(type: string, handler: () => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(handler);
    this.listeners.set(type, bucket);
  }
  fire(type: string): void {
    for (const handler of this.listeners.get(type) ?? []) handler();
  }
}

class FakeTransaction {
  error: unknown = null;
  private completed = false;
  private readonly listeners = new Map<string, Array<() => void>>();
  constructor(
    private readonly store: Map<IDBValidKey, unknown>,
    private readonly fault: FaultMode,
  ) {}
  addEventListener(type: string, handler: () => void): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(handler);
    this.listeners.set(type, bucket);
  }
  fire(type: string): void {
    for (const handler of this.listeners.get(type) ?? []) handler();
  }
  complete(): void {
    if (this.completed) return;
    this.completed = true;
    this.fire("complete");
  }
  objectStore(_name: string) {
    return {
      get: (key: IDBValidKey) => {
        const request = new FakeRequest();
        queueMicrotask(() => {
          if (this.fault === "get") {
            request.error = new Error("boom-get");
            request.fire("error");
            return;
          }
          request.result = this.store.has(key) ? this.store.get(key) : undefined;
          request.fire("success");
          queueMicrotask(() => this.complete());
        });
        return request;
      },
      put: (value: unknown, key: IDBValidKey) => {
        const request = new FakeRequest();
        queueMicrotask(() => {
          if (this.fault === "put") {
            this.error = new Error("boom-put");
            this.fire("error");
            return;
          }
          this.store.set(key, value);
          request.result = key;
          request.fire("success");
          this.complete();
        });
        return request;
      },
      delete: (key: IDBValidKey) => {
        const request = new FakeRequest();
        queueMicrotask(() => {
          if (this.fault === "delete") {
            this.error = new Error("boom-delete");
            this.fire("error");
            return;
          }
          this.store.delete(key);
          request.fire("success");
          this.complete();
        });
        return request;
      },
      openCursor: (range: { includes: (key: IDBValidKey) => boolean }) => {
        const request = new FakeRequest();
        queueMicrotask(() => {
          if (this.fault === "cursor") {
            request.error = new Error("boom-cursor");
            request.fire("error");
            return;
          }
          const keys = [...this.store.keys()]
            .filter((key) => range.includes(key))
            .sort() as IDBValidKey[];
          let index = 0;
          const step = () => {
            if (index >= keys.length) {
              request.result = null;
              request.fire("success");
              this.complete();
              return;
            }
            const key = keys[index++]!;
            request.result = {
              delete: () => {
                this.store.delete(key);
              },
              continue: () => {
                queueMicrotask(step);
              },
            };
            request.fire("success");
          };
          step();
        });
        return request;
      },
    };
  }
}

interface FakeDatabaseHandle {
  readonly db: IDBDatabase;
  readonly stores: Map<string, Map<IDBValidKey, unknown>>;
}

function makeFakeDatabase(fault: FaultMode = "none"): FakeDatabaseHandle {
  const stores = new Map<string, Map<IDBValidKey, unknown>>();
  const ensure = (name: string) => {
    const existing = stores.get(name);
    if (existing) return existing;
    const created = new Map<IDBValidKey, unknown>();
    stores.set(name, created);
    return created;
  };
  const db = {
    addEventListener: () => undefined,
    objectStoreNames: { contains: (name: string) => stores.has(name) },
    createObjectStore: (name: string) => {
      ensure(name);
      return {};
    },
    transaction: (storeName: string, _mode: string) =>
      new FakeTransaction(ensure(storeName), fault),
    close: () => undefined,
  } as unknown as IDBDatabase;
  return { db, stores };
}

type OpenMode = "success" | "error" | "undefined";

function installFakeIndexedDb(
  options: { open?: OpenMode; fault?: FaultMode } = {},
): FakeDatabaseHandle {
  const handle = makeFakeDatabase(options.fault ?? "none");
  const openMode = options.open ?? "success";
  if (openMode === "undefined") {
    vi.stubGlobal("indexedDB", undefined);
  } else {
    vi.stubGlobal("indexedDB", {
      open: (_name: string, _version: number) => {
        const request = new FakeRequest();
        queueMicrotask(() => {
          if (openMode === "error") {
            request.error = new Error("open-denied");
            request.fire("error");
            return;
          }
          request.result = handle.db;
          request.fire("upgradeneeded");
          request.fire("success");
        });
        return request;
      },
    });
  }
  vi.stubGlobal("IDBKeyRange", {
    bound: (lower: string, upper: string) => ({
      includes: (key: IDBValidKey) => typeof key === "string" && key >= lower && key <= upper,
    }),
  });
  vi.stubGlobal("window", {});
  return handle;
}

function openCatalogDatabase(factory: IDBFactory, databaseName: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = factory.open(databaseName, 1);
    request.addEventListener("upgradeneeded", () => {
      request.result.createObjectStore("catalog");
    });
    request.addEventListener("success", () => resolve(request.result));
    request.addEventListener("error", () => reject(request.error));
  });
}

// ── Domain fixtures ──────────────────────────────────────────────────

const environmentId = EnvironmentId.make("environment-1");
const otherEnvironmentId = EnvironmentId.make("environment-2");
const threadId = ThreadId.make("thread-1");
const projectId = ProjectId.make("project-1");
const connectionId = "connection-1";
const now = "2026-03-29T00:00:00.000Z";
const modelSelection = { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" } as const;
const decodeBearerConnectionProfile = Schema.decodeUnknownSync(BearerConnectionProfile);

function bearerRegistration(): BearerConnectionRegistration {
  return new BearerConnectionRegistration({
    target: new BearerConnectionTarget({
      environmentId,
      label: "Bearer backend",
      connectionId,
      serverEnvironmentId: null,
    }),
    profile: new BearerConnectionProfile({
      connectionId,
      environmentId,
      label: "Bearer backend",
      httpBaseUrl: "http://127.0.0.1:3201/",
      wsBaseUrl: "ws://127.0.0.1:3201/",
      hostKey: null,
    }),
    credential: new BearerConnectionCredential({ token: "bearer-token" }),
  });
}

function primaryPrepared(storageInstanceId: string | null): PreparedConnection {
  const target = new PrimaryConnectionTarget({
    environmentId,
    label: "Primary environment",
    httpBaseUrl: "https://primary.example.test",
    wsBaseUrl: "wss://primary.example.test",
  });
  return {
    environmentId: target.environmentId,
    label: target.label,
    descriptor: {
      environmentId: target.environmentId,
      label: target.label,
      platform: { os: "linux", arch: "x64" },
      serverVersion: "0.0.0-test",
      storageInstanceId,
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
    },
    httpBaseUrl: target.httpBaseUrl,
    socketUrl: `${target.wsBaseUrl}/ws`,
    httpAuthorization: null,
    e2ee: null,
    target,
  };
}

function shellSnapshot(): OrchestrationShellSnapshot {
  return { snapshotSequence: 0, projects: [], threads: [], updatedAt: now };
}

function orchestrationThread(): OrchestrationThread {
  return {
    id: threadId,
    projectId,
    title: "Demo Thread",
    modelSelection,
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    latestTurn: null,
    createdAt: now,
    updatedAt: now,
    archivedAt: null,
    deletedAt: null,
    messages: [],
    proposedPlans: [],
    activities: [],
    checkpoints: [],
    session: null,
  } as OrchestrationThread;
}

const remoteToken = new TokenStore.RemoteDpopAccessToken({
  environmentId,
  label: "Remote",
  endpoint: {
    httpBaseUrl: "https://relay.example/",
    wsBaseUrl: "wss://relay.example/",
    providerKind: "coddy_studio_relay",
  },
  accessToken: "remote-access-token",
  expiresAtEpochMs: 1_000,
  dpopThumbprint: "thumb",
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("decodes persisted legacy bearer profiles with no host key", () => {
  const decoded = decodeBearerConnectionProfile({
    _tag: "BearerConnectionProfile",
    connectionId: "bearer:legacy",
    environmentId: "environment-legacy",
    label: "Legacy",
    httpBaseUrl: "http://192.168.1.20:3773/",
    wsBaseUrl: "ws://192.168.1.20:3773/",
  });

  expect(decoded.hostKey).toBeNull();
});

// ─────────────────────────────────────────────────────────────────────
// makeCatalogStore
// ─────────────────────────────────────────────────────────────────────

describe("makeCatalogStore", () => {
  it.effect("quarantines a corrupt catalog without replacing it or accepting mutations", () =>
    Effect.gen(function* () {
      const writes: string[] = [];
      const quarantined: string[] = [];
      let durableCatalog = "{malformed";
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, next) =>
          Effect.sync(() => {
            if (durableCatalog !== expected) return false;
            durableCatalog = next;
            writes.push(next);
            return true;
          }),
        quarantine: (raw) => Effect.sync(() => quarantined.push(raw)),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
      expect(yield* SubscriptionRef.get(store.health)).toEqual({
        status: "recovery-required",
        message: "The connection catalog is corrupt and must be reset before it can be changed.",
      });
      expect(quarantined).toEqual(["{malformed"]);
      expect(writes).toEqual([]);
      expect(durableCatalog).toBe("{malformed");

      const error = yield* store.update((document) => document).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error.message).toContain("must be reset");
      expect(writes).toEqual([]);
      expect(durableCatalog).toBe("{malformed");
    }),
  );

  it.effect("only replaces a corrupt catalog after an explicit reset", () =>
    Effect.gen(function* () {
      let durableCatalog = "{malformed";
      const writes: string[] = [];
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, next) =>
          Effect.sync(() => {
            if (durableCatalog !== expected) return false;
            durableCatalog = next;
            writes.push(next);
            return true;
          }),
      });

      yield* store.read;
      expect(writes).toEqual([]);

      yield* store.reset;

      expect(writes).toHaveLength(1);
      expect(decodeCatalog(durableCatalog)).toEqual(emptyCatalog);
      expect(yield* SubscriptionRef.get(store.health)).toEqual({ status: "ready" });
    }),
  );

  it.effect("re-decides a compare-only transition after a conflicting revision", () =>
    Effect.gen(function* () {
      const firstCatalog = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "store-a" },
        ],
      });
      const winningCatalog = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "store-b" },
        ],
      });
      let durableCatalog = firstCatalog;
      let comparisons = 0;
      const writes = vi.fn(() => Effect.die(new Error("writer must not be called")));
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: (expected) =>
          Effect.sync(() => {
            comparisons += 1;
            if (comparisons === 1) {
              durableCatalog = winningCatalog;
              return false;
            }
            return durableCatalog === expected;
          }),
        compareAndSet: writes,
      });
      const observed: string[] = [];

      const result = yield* store.modify((document) => {
        const storageInstanceId = document.acceptedStorageIdentities[0]?.storageInstanceId ?? null;
        observed.push(storageInstanceId ?? "null");
        return { mutation: { _tag: "Keep" }, result: storageInstanceId };
      });

      expect(result).toBe("store-b");
      expect(observed).toEqual(["store-a", "store-b"]);
      expect(comparisons).toBe(2);
      expect(writes).not.toHaveBeenCalled();
    }),
  );

  it.effect("merges disjoint updates from independently constructed stores", () =>
    Effect.gen(function* () {
      let durableCatalog = encodeCatalog(emptyCatalog);
      const bothReadsComplete = yield* Deferred.make<void>();
      const releaseReads = yield* Deferred.make<void>();
      let reads = 0;
      const backend = {
        read: Effect.gen(function* () {
          const raw = durableCatalog;
          reads += 1;
          if (reads === 2) yield* Deferred.succeed(bothReadsComplete, undefined);
          if (reads <= 2) yield* Deferred.await(releaseReads);
          return raw;
        }),
        compare: unusedCatalogCompare,
        compareAndSet: (expected: string | null, raw: string) =>
          Effect.sync(() => {
            if (durableCatalog !== expected) return false;
            durableCatalog = raw;
            return true;
          }),
      };
      const first = yield* makeCatalogStore(backend);
      const second = yield* makeCatalogStore(backend);

      const firstUpdate = yield* Effect.forkChild(
        first.update((document) => ({
          ...document,
          acceptedStorageIdentities: [
            { targetKey: "platform:primary", storageInstanceId: "store-primary" },
          ],
        })),
        { startImmediately: true },
      );
      const secondUpdate = yield* Effect.forkChild(
        second.update((document) => ({
          ...document,
          acceptedStorageIdentities: [
            ...document.acceptedStorageIdentities,
            { targetKey: "bearer:remote", storageInstanceId: "store-remote" },
          ],
        })),
        { startImmediately: true },
      );
      yield* Deferred.await(bothReadsComplete);
      yield* Deferred.succeed(releaseReads, undefined);
      yield* Fiber.join(firstUpdate);
      yield* Fiber.join(secondUpdate);

      expect(decodeCatalog(durableCatalog).acceptedStorageIdentities).toEqual([
        { targetKey: "platform:primary", storageInstanceId: "store-primary" },
        { targetKey: "bearer:remote", storageInstanceId: "store-remote" },
      ]);
    }),
  );

  it.effect("reapplies the ordered winner for concurrent acceptance of the same target", () =>
    Effect.gen(function* () {
      const initial = encodeCatalog(emptyCatalog);
      let durableCatalog = initial;
      const firstAtCompare = yield* Deferred.make<void>();
      const releaseFirst = yield* Deferred.make<void>();
      const firstCommitted = yield* Deferred.make<void>();
      const compare = (expected: string | null, raw: string) =>
        Effect.gen(function* () {
          const identity = decodeCatalog(raw).acceptedStorageIdentities[0]?.storageInstanceId;
          if (expected === initial && identity === "store-first") {
            yield* Deferred.succeed(firstAtCompare, undefined);
            yield* Deferred.await(releaseFirst);
          } else if (expected === initial && identity === "store-second") {
            yield* Deferred.await(firstCommitted);
          }
          const updated = durableCatalog === expected;
          if (updated) durableCatalog = raw;
          if (identity === "store-first") yield* Deferred.succeed(firstCommitted, undefined);
          return updated;
        });
      const backend = {
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: compare,
      };
      const first = yield* makeCatalogStore(backend);
      const second = yield* makeCatalogStore(backend);
      const accept = (storageInstanceId: string) => (document: ConnectionCatalogDocumentType) => ({
        ...document,
        acceptedStorageIdentities: [{ targetKey: "platform:primary", storageInstanceId }],
      });

      const firstUpdate = yield* Effect.forkChild(first.update(accept("store-first")), {
        startImmediately: true,
      });
      yield* Deferred.await(firstAtCompare);
      const secondUpdate = yield* Effect.forkChild(second.update(accept("store-second")), {
        startImmediately: true,
      });
      yield* Deferred.succeed(releaseFirst, undefined);
      yield* Fiber.join(firstUpdate);
      yield* Fiber.join(secondUpdate);

      expect(decodeCatalog(durableCatalog).acceptedStorageIdentities).toEqual([
        { targetKey: "platform:primary", storageInstanceId: "store-second" },
      ]);
    }),
  );

  it.effect(
    "preserves registration, profile, credential, and DPoP token fields across an acceptance race",
    () =>
      Effect.gen(function* () {
        let durableCatalog = encodeCatalog(emptyCatalog);
        const bothReadsComplete = yield* Deferred.make<void>();
        const releaseReads = yield* Deferred.make<void>();
        let reads = 0;
        const backend = {
          read: Effect.gen(function* () {
            const raw = durableCatalog;
            reads += 1;
            if (reads === 2) yield* Deferred.succeed(bothReadsComplete, undefined);
            if (reads <= 2) yield* Deferred.await(releaseReads);
            return raw;
          }),
          compare: unusedCatalogCompare,
          compareAndSet: (expected: string | null, raw: string) =>
            Effect.sync(() => {
              if (durableCatalog !== expected) return false;
              durableCatalog = raw;
              return true;
            }),
        };
        const identityStore = yield* makeCatalogStore(backend);
        const connectionStore = yield* makeCatalogStore(backend);
        const registration = bearerRegistration();
        const identityUpdate = yield* Effect.forkChild(
          identityStore.update((document) => ({
            ...document,
            acceptedStorageIdentities: [
              { targetKey: "platform:primary", storageInstanceId: "store-primary" },
            ],
          })),
          { startImmediately: true },
        );
        const connectionUpdate = yield* Effect.forkChild(
          connectionStore.update((document) => ({
            ...document,
            targets: [registration.target],
            profiles: [registration.profile],
            credentials: [
              {
                connectionId: registration.profile.connectionId,
                credential: registration.credential,
              },
            ],
            remoteDpopTokens: [remoteToken],
          })),
          { startImmediately: true },
        );
        yield* Deferred.await(bothReadsComplete);
        yield* Deferred.succeed(releaseReads, undefined);
        yield* Fiber.join(identityUpdate);
        yield* Fiber.join(connectionUpdate);

        const document = decodeCatalog(durableCatalog);
        expect(document.acceptedStorageIdentities).toHaveLength(1);
        expect(document.targets).toEqual([registration.target]);
        expect(document.profiles).toEqual([registration.profile]);
        expect(document.credentials).toHaveLength(1);
        expect(document.remoteDpopTokens).toEqual([remoteToken]);
      }),
  );

  it.effect("rereads after an explicit corrupt-catalog reset loses a compare-and-set race", () =>
    Effect.gen(function* () {
      const newer = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "newer-store" },
        ],
      });
      let durableCatalog = "{not-json";
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, _next) =>
          Effect.sync(() => {
            if (expected === "{not-json") durableCatalog = newer;
            return false;
          }),
      });

      yield* store.read;
      yield* store.reset;
      expect((yield* store.read).acceptedStorageIdentities).toEqual([
        { targetKey: "platform:primary", storageInstanceId: "newer-store" },
      ]);
    }),
  );

  it.effect("finishes an interrupted reset by observing the durable compare-and-set winner", () =>
    Effect.gen(function* () {
      const winner = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "winner-store" },
        ],
      });
      let durableCatalog = "{not-json";
      const compareStarted = yield* Deferred.make<void>();
      const releaseCompare = yield* Deferred.make<void>();
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, next) =>
          Effect.gen(function* () {
            yield* Deferred.succeed(compareStarted, undefined);
            yield* Deferred.await(releaseCompare);
            if (durableCatalog !== expected) return false;
            durableCatalog = next;
            return true;
          }),
      });

      yield* store.read;
      const reset = yield* Effect.forkChild(store.reset, { startImmediately: true });
      yield* Deferred.await(compareStarted);
      durableCatalog = winner;
      const interruption = yield* Effect.forkChild(Fiber.interrupt(reset), {
        startImmediately: true,
      });
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseCompare, undefined);
      yield* Fiber.join(interruption);

      expect(durableCatalog).toBe(winner);
      expect(yield* SubscriptionRef.get(store.health)).toEqual({ status: "ready" });
      expect((yield* store.read).acceptedStorageIdentities).toEqual([
        { targetKey: "platform:primary", storageInstanceId: "winner-store" },
      ]);
    }),
  );

  it.effect("fails with a typed error after bounded compare-and-set conflicts", () =>
    Effect.gen(function* () {
      let attempts = 0;
      const store = yield* makeCatalogStore({
        read: Effect.succeed(encodeCatalog(emptyCatalog)),
        compare: unusedCatalogCompare,
        compareAndSet: () =>
          Effect.sync(() => {
            attempts += 1;
            return false;
          }),
      });

      const error = yield* store.update((document) => document).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error.message).toContain("changed too many times");
      expect(attempts).toBe(8);
    }),
  );

  it.effect("reloads a durable commit when its updating fiber is interrupted", () =>
    Effect.gen(function* () {
      let durableCatalog = encodeCatalog(emptyCatalog);
      const committed = yield* Deferred.make<void>();
      const releaseCommit = yield* Deferred.make<void>();
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, raw) =>
          Effect.sync(() => {
            if (durableCatalog !== expected) return false;
            durableCatalog = raw;
            return true;
          }).pipe(
            Effect.tap(() => Deferred.succeed(committed, undefined)),
            Effect.tap(() => Deferred.await(releaseCommit)),
          ),
      });

      yield* store.read;
      const update = yield* Effect.forkChild(
        store.update((document) => ({
          ...document,
          acceptedStorageIdentities: [
            { targetKey: "platform:primary", storageInstanceId: "store-primary" },
          ],
        })),
        { startImmediately: true },
      );
      yield* Deferred.await(committed);
      const interruption = yield* Effect.forkChild(Fiber.interrupt(update), {
        startImmediately: true,
      });
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseCommit, undefined);
      yield* Fiber.join(interruption);

      expect(decodeCatalog(durableCatalog).acceptedStorageIdentities).toHaveLength(1);
      expect((yield* store.read).acceptedStorageIdentities).toEqual([
        { targetKey: "platform:primary", storageInstanceId: "store-primary" },
      ]);
    }),
  );

  it.effect("quarantines malformed catalogs and keeps only an in-memory empty document", () =>
    Effect.gen(function* () {
      const writes: string[] = [];
      const quarantined: string[] = [];
      const store = yield* makeCatalogStore({
        read: Effect.succeed("{not-json"),
        compare: unusedCatalogCompare,
        compareAndSet: (_expected, raw) =>
          Effect.sync(() => {
            writes.push(raw);
            return true;
          }),
        quarantine: (raw) => Effect.sync(() => quarantined.push(raw)),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
      expect(quarantined).toEqual(["{not-json"]);
      expect(writes).toEqual([]);
    }),
  );

  it.effect("recovers when corrupt-catalog quarantine and replacement persistence both fail", () =>
    Effect.gen(function* () {
      const quarantineFailure = new ConnectionTransientError({
        reason: "remote-unavailable",
        detail: "quarantine unavailable",
      });
      const writeFailure = new ConnectionTransientError({
        reason: "remote-unavailable",
        detail: "replacement unavailable",
      });
      const store = yield* makeCatalogStore({
        read: Effect.succeed("{not-json"),
        compare: unusedCatalogCompare,
        compareAndSet: () => Effect.fail(writeFailure),
        quarantine: () => Effect.fail(quarantineFailure),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
      expect(yield* store.read).toEqual(emptyCatalog);
    }),
  );

  it.effect("does not hide catalog read failures", () =>
    Effect.gen(function* () {
      const failure = new ConnectionTransientError({
        reason: "remote-unavailable",
        detail: "permission denied",
      });
      const store = yield* makeCatalogStore({
        read: Effect.fail(failure),
        compare: unusedCatalogCompare,
        compareAndSet: () => Effect.succeed(true),
      });

      expect(yield* Effect.flip(store.read)).toBe(failure);
    }),
  );

  it.effect("reads an empty document when the backend has no stored catalog", () =>
    Effect.gen(function* () {
      let reads = 0;
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => {
          reads += 1;
          return null;
        }),
        compare: unusedCatalogCompare,
        compareAndSet: () => Effect.succeed(true),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
      expect(yield* store.read).toEqual(emptyCatalog);
      expect(reads).toBe(2);
    }),
  );

  it.effect("treats a blank stored catalog as empty", () =>
    Effect.gen(function* () {
      const store = yield* makeCatalogStore({
        read: Effect.succeed("   "),
        compare: unusedCatalogCompare,
        compareAndSet: () => Effect.succeed(true),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
    }),
  );

  it.effect("update transforms, encodes, persists, and reloads the next document", () =>
    Effect.gen(function* () {
      const writes: string[] = [];
      let durableCatalog: string | null = null;
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => durableCatalog),
        compare: unusedCatalogCompare,
        compareAndSet: (expected, raw) =>
          Effect.sync(() => {
            if (durableCatalog !== expected) return false;
            durableCatalog = raw;
            writes.push(raw);
            return true;
          }),
      });

      yield* store.update((document) => ({
        ...document,
        profiles: [bearerRegistration().profile],
      }));

      expect(writes).toHaveLength(1);
      const persisted = decodeCatalog(writes[0]!);
      expect(persisted.profiles).toHaveLength(1);
      expect((yield* store.read).profiles).toHaveLength(1);
    }),
  );

  it.effect("decodes a fresh well-formed stored catalog on every read", () => {
    const encoded = encodeCatalog(emptyCatalog);
    return Effect.gen(function* () {
      let reads = 0;
      const store = yield* makeCatalogStore({
        read: Effect.sync(() => {
          reads += 1;
          return encoded;
        }),
        compare: unusedCatalogCompare,
        compareAndSet: () => Effect.succeed(true),
      });

      expect(yield* store.read).toEqual(emptyCatalog);
      yield* store.read;
      expect(reads).toBe(2);
    });
  });
});

// ─────────────────────────────────────────────────────────────────────
// makeCatalogBackend
// ─────────────────────────────────────────────────────────────────────

describe("connectionCatalogLivesInIndexedDb", () => {
  it("is true without a desktop bridge and false on a protected desktop", () => {
    vi.stubGlobal("window", {});
    expect(connectionCatalogLivesInIndexedDb()).toBe(true);

    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(),
        compareAndSetConnectionCatalog: vi.fn(),
      },
    });
    expect(connectionCatalogLivesInIndexedDb()).toBe(false);

    // A bridge missing either catalog method still routes the catalog to
    // IndexedDB.
    vi.stubGlobal("window", { desktopBridge: { getConnectionCatalog: vi.fn() } });
    expect(connectionCatalogLivesInIndexedDb()).toBe(true);
  });
});

describe("makeCatalogBackend (desktop bridge)", () => {
  it.effect("reads and compares through the desktop bridge secure storage", () =>
    Effect.gen(function* () {
      const compareConnectionCatalog = vi.fn().mockResolvedValue(true);
      const compareAndSetConnectionCatalog = vi.fn().mockResolvedValue(true);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue("stored-catalog"),
          compareConnectionCatalog,
          compareAndSetConnectionCatalog,
        },
      });
      const backend = makeCatalogBackend({} as IDBDatabase);

      expect(yield* backend.read).toBe("stored-catalog");
      expect(yield* backend.compare("stored-catalog")).toBe(true);
      expect(yield* backend.compareAndSet("stored-catalog", "payload")).toBe(true);
      expect(compareConnectionCatalog).toHaveBeenCalledWith("stored-catalog");
      expect(compareAndSetConnectionCatalog).toHaveBeenCalledWith("stored-catalog", "payload");
      // The bridge backend does not expose a quarantine seam.
      expect(backend.quarantine).toBeUndefined();
    }),
  );

  it.effect("fails comparison closed when a protected bridge lacks compare-only support", () =>
    Effect.gen(function* () {
      const compareAndSetConnectionCatalog = vi.fn().mockResolvedValue(true);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue("stored-catalog"),
          compareAndSetConnectionCatalog,
        },
      });
      const backend = makeCatalogBackend({} as IDBDatabase);

      const error = yield* Effect.flip(backend.compare("stored-catalog"));

      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error.message).toContain("comparison is unavailable");
      expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    }),
  );

  it.effect("maps desktop bridge read rejections to a transient error", () =>
    Effect.gen(function* () {
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockRejectedValue(new Error("locked")),
          compareAndSetConnectionCatalog: vi.fn().mockResolvedValue(true),
        },
      });
      const backend = makeCatalogBackend({} as IDBDatabase);

      const error = yield* backend.read.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(error.message).toContain("load the local connection catalog");
    }),
  );

  it.effect("selects IndexedDB when the desktop bridge has no protected CAS capability", () =>
    Effect.gen(function* () {
      const handle = installFakeIndexedDb();
      const setConnectionCatalog = vi.fn().mockResolvedValue(false);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue(null),
          setConnectionCatalog,
        },
      });
      const backend = makeCatalogBackend(handle.db);

      expect(yield* backend.compareAndSet(null, "{}")).toBe(true);

      expect(yield* backend.read).toBe("{}");
      expect(setConnectionCatalog).not.toHaveBeenCalled();
    }),
  );
});

describe("makeCatalogBackend (IndexedDB)", () => {
  it.effect("compares existing and absent revisions without putting a document", () =>
    Effect.gen(function* () {
      vi.stubGlobal("window", {});
      const factory = new IDBFactory();
      const existingDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-compare-existing"),
      );
      const absentDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-compare-absent"),
      );
      const existingBackend = makeCatalogBackend(existingDatabase);
      const absentBackend = makeCatalogBackend(absentDatabase);
      const existingRaw = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "store-a" },
        ],
      });
      expect(yield* existingBackend.compareAndSet(null, existingRaw)).toBe(true);
      const put = vi.spyOn(IDBObjectStore.prototype, "put");
      const existingStore = yield* makeCatalogStore(existingBackend);
      const absentStore = yield* makeCatalogStore(absentBackend);

      expect(
        yield* existingStore.modify((document) => ({
          mutation: { _tag: "Keep" },
          result: document.acceptedStorageIdentities[0]?.storageInstanceId ?? null,
        })),
      ).toBe("store-a");
      expect(
        yield* absentStore.modify(() => ({
          mutation: { _tag: "Keep" },
          result: "absent",
        })),
      ).toBe("absent");

      expect(put).not.toHaveBeenCalled();
      expect(yield* existingBackend.read).toBe(existingRaw);
      expect(yield* absentBackend.read).toBeNull();
      existingDatabase.close();
      absentDatabase.close();
    }),
  );

  it.effect("re-decides compare-only state after a concurrent IndexedDB writer", () =>
    Effect.gen(function* () {
      vi.stubGlobal("window", {});
      const factory = new IDBFactory();
      const firstDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-compare-race"),
      );
      const secondDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-compare-race"),
      );
      const firstBackend = makeCatalogBackend(firstDatabase);
      const secondBackend = makeCatalogBackend(secondDatabase);
      const initial = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "store-a" },
        ],
      });
      expect(yield* firstBackend.compareAndSet(null, initial)).toBe(true);
      const compareStarted = yield* Deferred.make<void>();
      const releaseCompare = yield* Deferred.make<void>();
      let comparisons = 0;
      const compareBackend = {
        ...firstBackend,
        compare: (expected: string | null) =>
          Effect.sync(() => {
            comparisons += 1;
            return comparisons;
          }).pipe(
            Effect.flatMap((attempt) =>
              attempt === 1
                ? Deferred.succeed(compareStarted, undefined).pipe(
                    Effect.andThen(Deferred.await(releaseCompare)),
                  )
                : Effect.void,
            ),
            Effect.andThen(firstBackend.compare(expected)),
          ),
      };
      const compareStore = yield* makeCatalogStore(compareBackend);
      const writerStore = yield* makeCatalogStore(secondBackend);
      const observed: string[] = [];
      const put = vi.spyOn(IDBObjectStore.prototype, "put");
      const comparison = yield* Effect.forkChild(
        compareStore.modify((document) => {
          const storageInstanceId =
            document.acceptedStorageIdentities[0]?.storageInstanceId ?? null;
          observed.push(storageInstanceId ?? "null");
          return { mutation: { _tag: "Keep" }, result: storageInstanceId };
        }),
        { startImmediately: true },
      );
      yield* Deferred.await(compareStarted);

      yield* writerStore.update((document) => ({
        ...document,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "store-b" },
        ],
      }));
      yield* Deferred.succeed(releaseCompare, undefined);

      expect(yield* Fiber.join(comparison)).toBe("store-b");
      expect(observed).toEqual(["store-a", "store-b"]);
      expect(comparisons).toBe(2);
      expect(put).toHaveBeenCalledTimes(1);
      firstDatabase.close();
      secondDatabase.close();
    }),
  );

  it.effect("atomically merges conflicting updates from two database connections", () =>
    Effect.gen(function* () {
      vi.stubGlobal("window", {});
      const factory = new IDBFactory();
      const firstDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-concurrency"),
      );
      const secondDatabase = yield* Effect.promise(() =>
        openCatalogDatabase(factory, "catalog-concurrency"),
      );
      const firstBackend = makeCatalogBackend(firstDatabase);
      const secondBackend = makeCatalogBackend(secondDatabase);
      const initial = encodeCatalog(emptyCatalog);
      expect(yield* firstBackend.compareAndSet(null, initial)).toBe(true);

      const bothReadsComplete = yield* Deferred.make<void>();
      const releaseReads = yield* Deferred.make<void>();
      let reads = 0;
      const gateInitialRead = (backend: typeof firstBackend) => ({
        ...backend,
        read: Effect.gen(function* () {
          const raw = yield* backend.read;
          reads += 1;
          if (reads === 2) yield* Deferred.succeed(bothReadsComplete, undefined);
          if (reads <= 2) yield* Deferred.await(releaseReads);
          return raw;
        }),
      });
      const firstStore = yield* makeCatalogStore(gateInitialRead(firstBackend));
      const secondStore = yield* makeCatalogStore(gateInitialRead(secondBackend));
      const firstUpdate = yield* Effect.forkChild(
        firstStore.update((document) => ({
          ...document,
          acceptedStorageIdentities: [
            ...document.acceptedStorageIdentities,
            { targetKey: "platform:primary", storageInstanceId: "store-primary" },
          ],
        })),
        { startImmediately: true },
      );
      const secondUpdate = yield* Effect.forkChild(
        secondStore.update((document) => ({
          ...document,
          acceptedStorageIdentities: [
            ...document.acceptedStorageIdentities,
            { targetKey: "bearer:remote", storageInstanceId: "store-remote" },
          ],
        })),
        { startImmediately: true },
      );

      yield* Deferred.await(bothReadsComplete);
      yield* Deferred.succeed(releaseReads, undefined);
      yield* Fiber.join(firstUpdate);
      yield* Fiber.join(secondUpdate);

      const raw = yield* firstBackend.read;
      expect(decodeCatalog(raw!).acceptedStorageIdentities).toEqual(
        expect.arrayContaining([
          { targetKey: "platform:primary", storageInstanceId: "store-primary" },
          { targetKey: "bearer:remote", storageInstanceId: "store-remote" },
        ]),
      );
      expect(decodeCatalog(raw!).acceptedStorageIdentities).toHaveLength(2);
      firstDatabase.close();
      secondDatabase.close();
    }),
  );

  it.effect("reads null when the catalog store is empty, then round-trips a compare-and-set", () =>
    Effect.gen(function* () {
      const handle = installFakeIndexedDb();
      const backend = makeCatalogBackend(handle.db);

      expect(yield* backend.read).toBeNull();
      expect(yield* backend.compareAndSet(null, "catalog-json")).toBe(true);
      expect(yield* backend.read).toBe("catalog-json");

      yield* backend.quarantine!("corrupt-json");
      const catalogStore = handle.stores.get("catalog")!;
      const quarantineKey = [...catalogStore.keys()].find(
        (key) => typeof key === "string" && key.startsWith("document:corrupt:"),
      );
      expect(quarantineKey).toBeDefined();
    }),
  );

  it.effect("maps IndexedDB read failures to a transient error", () =>
    Effect.gen(function* () {
      const handle = installFakeIndexedDb({ fault: "get" });
      const backend = makeCatalogBackend(handle.db);

      const error = yield* backend.read.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }),
  );

  it.effect("maps IndexedDB compare-and-set failures to a transient error", () =>
    Effect.gen(function* () {
      const handle = installFakeIndexedDb({ fault: "put" });
      const backend = makeCatalogBackend(handle.db);

      const error = yield* backend.compareAndSet(null, "payload").pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }),
  );
});

// ─────────────────────────────────────────────────────────────────────
// connectionStorageLayer (end-to-end over the fake IndexedDB)
// ─────────────────────────────────────────────────────────────────────

describe("connectionStorageLayer", () => {
  it.effect("rolls back only the matching accepted storage identity", () => {
    installFakeIndexedDb();
    vi.stubGlobal("window", {});

    return Effect.gen(function* () {
      const identities = yield* AcceptedStorageIdentityStore;
      const first = { targetKey: "bearer:test", storageInstanceId: "store-a" };
      yield* identities.accept(first);
      const removed = yield* identities.rollbackAcceptance(first, null);
      const afterRemoval = yield* identities.get(first.targetKey);
      expect(removed).toBe(true);
      expect(Option.isNone(afterRemoval)).toBe(true);

      yield* identities.accept({ ...first, storageInstanceId: "store-previous" });
      yield* identities.accept(first);
      const restored = yield* identities.rollbackAcceptance(first, "store-previous");
      const afterRestore = yield* identities.get(first.targetKey);
      expect(restored).toBe(true);
      expect(afterRestore).toEqual(Option.some("store-previous"));

      yield* identities.accept(first);
      yield* identities.accept({ ...first, storageInstanceId: "store-concurrent" });
      const preserved = yield* identities.rollbackAcceptance(first, "store-previous");
      const afterReplacement = yield* identities.get(first.targetKey);
      expect(preserved).toBe(false);
      expect(afterReplacement).toEqual(Option.some("store-concurrent"));
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("publishes corrupt catalog health and resets only through the explicit service", () => {
    const handle = installFakeIndexedDb();
    handle.stores.set("catalog", new Map([["document", "{malformed"]]));

    return Effect.gen(function* () {
      const targets = yield* ConnectionTargetStore;
      const health = yield* ConnectionCatalogHealthStore;

      expect(yield* targets.list).toEqual([]);
      expect(yield* SubscriptionRef.get(health.state)).toMatchObject({
        status: "recovery-required",
      });
      expect(handle.stores.get("catalog")?.get("document")).toBe("{malformed");

      yield* health.reset;

      expect(yield* SubscriptionRef.get(health.state)).toEqual({ status: "ready" });
      expect(decodeCatalog(handle.stores.get("catalog")?.get("document") as string)).toEqual(
        emptyCatalog,
      );
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("keeps an accepted identity without invoking the catalog writer", () => {
    installFakeIndexedDb();
    const storedCatalog = encodeCatalog({
      ...emptyCatalog,
      acceptedStorageIdentities: [{ targetKey: "platform:primary", storageInstanceId: "store-a" }],
    });
    const compareConnectionCatalog = vi.fn((expected: string | null) =>
      Promise.resolve(expected === storedCatalog),
    );
    const compareAndSetConnectionCatalog = vi.fn(() =>
      Promise.reject(new Error("writer must not be called")),
    );
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(() => Promise.resolve(storedCatalog)),
        compareConnectionCatalog,
        compareAndSetConnectionCatalog,
      },
    });

    return Effect.gen(function* () {
      const identities = yield* AcceptedStorageIdentityStore;
      const decision = yield* identities.transition("platform:primary", (accepted) => ({
        result: decideStorageIdentity(accepted, "store-a"),
        mutation: { _tag: "Keep" },
      }));

      expect(decision).toEqual({ _tag: "Accepted", value: "store-a" });
      expect(compareConnectionCatalog).toHaveBeenCalledWith(storedCatalog);
      expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("reports a changed identity when the catalog writer is unavailable", () => {
    installFakeIndexedDb();
    const storedCatalog = encodeCatalog({
      ...emptyCatalog,
      acceptedStorageIdentities: [{ targetKey: "platform:primary", storageInstanceId: "store-a" }],
    });
    const compareAndSetConnectionCatalog = vi.fn(() =>
      Promise.reject(new Error("writer must not be called")),
    );
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(() => Promise.resolve(storedCatalog)),
        compareConnectionCatalog: vi.fn((expected: string | null) =>
          Promise.resolve(expected === storedCatalog),
        ),
        compareAndSetConnectionCatalog,
      },
    });

    return Effect.gen(function* () {
      const result = yield* verifyPreparedStorageIdentity(primaryPrepared("store-b")).pipe(
        Effect.result,
      );

      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.failure).toMatchObject({
          _tag: "ConnectionStorageChangedError",
          acceptedStorageInstanceId: "store-a",
          reportedStorageInstanceId: "store-b",
        });
      }
      expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("keeps nullable reports without creating or rewriting a catalog", () => {
    const indexedDb = installFakeIndexedDb();
    let storedCatalog: string | null = encodeCatalog({
      ...emptyCatalog,
      acceptedStorageIdentities: [{ targetKey: "platform:primary", storageInstanceId: "store-a" }],
    });
    const compareAndSetConnectionCatalog = vi.fn(() =>
      Promise.reject(new Error("writer must not be called")),
    );
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(() => Promise.resolve(storedCatalog)),
        compareConnectionCatalog: vi.fn((expected: string | null) =>
          Promise.resolve(expected === storedCatalog),
        ),
        compareAndSetConnectionCatalog,
      },
    });

    return Effect.gen(function* () {
      const identities = yield* AcceptedStorageIdentityStore;
      const keepNullable = (accepted: string | null) => ({
        result: decideStorageIdentity(accepted, null),
        mutation: { _tag: "Keep" as const },
      });

      const existingDecision = yield* identities.transition("platform:primary", keepNullable);
      storedCatalog = null;
      const absentDecision = yield* identities.transition("platform:primary", keepNullable);

      expect(existingDecision).toEqual({ _tag: "Unverifiable", accepted: "store-a" });
      expect(absentDecision).toEqual({ _tag: "Unverifiable", accepted: null });
      expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
      expect(indexedDb.stores.get("catalog")?.has("document") ?? false).toBe(false);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("elects one bootstrap winner across two real IndexedDB storage layers", () => {
    const factory = new IDBFactory();
    vi.stubGlobal("indexedDB", factory);
    vi.stubGlobal("IDBKeyRange", IDBKeyRange);
    vi.stubGlobal("window", {});
    let arrivals = 0;
    let reportBothArrived = () => {};
    let releaseBoth = () => {};
    const bothArrived = new Promise<void>((resolve) => {
      reportBothArrived = resolve;
    });
    const bothReleased = new Promise<void>((resolve) => {
      releaseBoth = resolve;
    });
    const gate = Effect.promise(async () => {
      arrivals += 1;
      if (arrivals === 2) reportBothArrived();
      await bothReleased;
    });
    const makePrepared = (suffix: string, storageInstanceId: string): PreparedConnection => {
      const target = new PrimaryConnectionTarget({
        environmentId: EnvironmentId.make(`environment-${suffix}`),
        label: `Environment ${suffix}`,
        httpBaseUrl: `https://${suffix}.example.test`,
        wsBaseUrl: `wss://${suffix}.example.test`,
      });
      return {
        environmentId: target.environmentId,
        label: target.label,
        descriptor: {
          environmentId: target.environmentId,
          label: target.label,
          platform: { os: "linux", arch: "x64" },
          serverVersion: "0.0.0-test",
          storageInstanceId,
          bootId: null,
          remoteUpdateSupport: null,
          remoteProtocolVersion: 1,
          minCompatibleRemoteProtocol: 1,
          capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
        },
        httpBaseUrl: target.httpBaseUrl,
        socketUrl: `${target.wsBaseUrl}/ws`,
        httpAuthorization: null,
        e2ee: null,
        target,
      };
    };
    const verify = (prepared: PreparedConnection) =>
      Effect.gen(function* () {
        const base = yield* AcceptedStorageIdentityStore;
        const atomicBase = base as typeof base & {
          readonly transition: <A>(
            targetKey: string,
            decide: (acceptedStorageInstanceId: string | null) => {
              readonly result: A;
              readonly mutation:
                | { readonly _tag: "Keep" }
                | { readonly _tag: "Set"; readonly storageInstanceId: string };
            },
          ) => Effect.Effect<A>;
        };
        const gatedService = {
          ...base,
          get: (targetKey: string) => base.get(targetKey).pipe(Effect.tap(() => gate)),
          transition: <A>(
            targetKey: string,
            decide: (acceptedStorageInstanceId: string | null) => {
              readonly result: A;
              readonly mutation:
                | { readonly _tag: "Keep" }
                | { readonly _tag: "Set"; readonly storageInstanceId: string };
            },
          ) => gate.pipe(Effect.andThen(atomicBase.transition(targetKey, decide))),
        };
        return yield* verifyPreparedStorageIdentity(prepared).pipe(
          Effect.provideService(
            AcceptedStorageIdentityStore,
            AcceptedStorageIdentityStore.of(gatedService),
          ),
        );
      }).pipe(Effect.provide(connectionStorageLayer), Effect.result);

    return Effect.gen(function* () {
      const first = yield* Effect.forkChild(verify(makePrepared("first", "store-a")), {
        startImmediately: true,
      });
      const second = yield* Effect.forkChild(verify(makePrepared("second", "store-b")), {
        startImmediately: true,
      });
      yield* Effect.promise(() => bothArrived);
      releaseBoth();
      const results = [yield* Fiber.join(first), yield* Fiber.join(second)];

      expect(results.filter((result) => result._tag === "Success")).toHaveLength(1);
      expect(results.filter((result) => result._tag === "Failure")).toHaveLength(1);
      const failure = results.find((result) => result._tag === "Failure");
      expect(failure?._tag === "Failure" ? failure.failure : null).toBeInstanceOf(
        ConnectionStorageChangedError,
      );
      const accepted = yield* Effect.gen(function* () {
        const identities = yield* AcceptedStorageIdentityStore;
        return yield* identities.get("platform:primary");
      }).pipe(Effect.provide(connectionStorageLayer));
      expect(Option.isSome(accepted)).toBe(true);
      if (Option.isSome(accepted)) {
        expect(["store-a", "store-b"]).toContain(accepted.value);
        if (
          failure?._tag === "Failure" &&
          failure.failure._tag === "ConnectionStorageChangedError"
        ) {
          expect(failure.failure.acceptedStorageInstanceId).toBe(accepted.value);
          expect(failure.failure.reportedStorageInstanceId).not.toBe(accepted.value);
        }
      }
    });
  });

  it.effect(
    "migrates a non-Windows desktop fallback catalog and updates it atomically in IndexedDB",
    () => {
      const handle = installFakeIndexedDb();
      const legacyCatalog = encodeCatalog(emptyCatalog);
      const clearConnectionCatalog = vi.fn().mockResolvedValue(undefined);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue(legacyCatalog),
          clearConnectionCatalog,
        },
      });

      return Effect.gen(function* () {
        const identityStore = yield* AcceptedStorageIdentityStore;
        yield* identityStore.accept({
          targetKey: "platform:primary",
          storageInstanceId: "store-macos",
        });

        const raw = handle.stores.get("catalog")?.get("document");
        expect(typeof raw).toBe("string");
        expect(decodeCatalog(raw as string).acceptedStorageIdentities).toEqual([
          { targetKey: "platform:primary", storageInstanceId: "store-macos" },
        ]);
        expect(clearConnectionCatalog).toHaveBeenCalledTimes(1);
      }).pipe(Effect.provide(connectionStorageLayer));
    },
  );

  it.effect(
    "preserves the exact IndexedDB winner when legacy migration loses its compare-and-set",
    () => {
      const handle = installFakeIndexedDb();
      const registration = bearerRegistration();
      const indexedDbWinner = encodeCatalog({
        ...emptyCatalog,
        targets: [registration.target],
        profiles: [registration.profile],
        credentials: [
          {
            connectionId: registration.target.connectionId,
            credential: registration.credential,
          },
        ],
        remoteDpopTokens: [remoteToken],
        acceptedStorageIdentities: [
          { targetKey: "bearer:connection-1", storageInstanceId: "indexeddb-winner" },
        ],
      });
      handle.stores.set("catalog", new Map([["document", indexedDbWinner]]));
      const legacyCatalog = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "legacy-loser" },
        ],
      });
      const clearConnectionCatalog = vi.fn().mockResolvedValue(undefined);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue(legacyCatalog),
          clearConnectionCatalog,
        },
      });

      return Effect.gen(function* () {
        const identityStore = yield* AcceptedStorageIdentityStore;

        expect(yield* identityStore.get("bearer:connection-1")).toEqual(
          Option.some("indexeddb-winner"),
        );
        expect(handle.stores.get("catalog")?.get("document")).toBe(indexedDbWinner);
        expect(clearConnectionCatalog).toHaveBeenCalledTimes(1);
      }).pipe(Effect.provide(connectionStorageLayer));
    },
  );

  it.effect(
    "replaces a corrupt IndexedDB value with the only valid legacy catalog before clearing it",
    () => {
      const handle = installFakeIndexedDb();
      const corruptIndexedDbCatalog = "{corrupt-indexeddb-catalog";
      handle.stores.set("catalog", new Map([["document", corruptIndexedDbCatalog]]));
      const legacyCatalog = encodeCatalog({
        ...emptyCatalog,
        acceptedStorageIdentities: [
          { targetKey: "platform:primary", storageInstanceId: "valid-legacy" },
        ],
      });
      const clearConnectionCatalog = vi.fn().mockResolvedValue(undefined);
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn().mockResolvedValue(legacyCatalog),
          clearConnectionCatalog,
        },
      });

      return Effect.gen(function* () {
        const identityStore = yield* AcceptedStorageIdentityStore;

        expect(yield* identityStore.get("platform:primary")).toEqual(Option.some("valid-legacy"));
        expect(handle.stores.get("catalog")?.get("document")).toBe(legacyCatalog);
        expect(
          [...(handle.stores.get("catalog")?.entries() ?? [])].some(
            ([key, value]) =>
              typeof key === "string" &&
              key.startsWith("document:corrupt:") &&
              value === corruptIndexedDbCatalog,
          ),
        ).toBe(true);
        expect(clearConnectionCatalog).toHaveBeenCalledTimes(1);
      }).pipe(Effect.provide(connectionStorageLayer));
    },
  );

  it.effect(
    "merges mutations from two independent desktop storage layers through bridge CAS",
    () => {
      installFakeIndexedDb();
      let storedCatalog = encodeCatalog(emptyCatalog);
      let reads = 0;
      let releaseReads = () => {};
      let reportBothReads = () => {};
      const bothReads = new Promise<void>((resolve) => {
        reportBothReads = resolve;
      });
      const readsReleased = new Promise<void>((resolve) => {
        releaseReads = resolve;
      });
      const compareAndSetConnectionCatalog = vi.fn(
        async (expected: string | null, next: string) => {
          if (storedCatalog !== expected) return false;
          storedCatalog = next;
          return true;
        },
      );
      vi.stubGlobal("window", {
        desktopBridge: {
          getConnectionCatalog: vi.fn(async () => {
            const raw = storedCatalog;
            reads += 1;
            if (reads === 2) reportBothReads();
            if (reads <= 2) await readsReleased;
            return raw;
          }),
          compareAndSetConnectionCatalog,
        },
      });

      const acceptIdentity = Effect.gen(function* () {
        const identityStore = yield* AcceptedStorageIdentityStore;
        yield* identityStore.accept({
          targetKey: "platform:primary",
          storageInstanceId: "store-desktop",
        });
      }).pipe(Effect.provide(connectionStorageLayer));
      const registerConnection = Effect.gen(function* () {
        const registrationStore = yield* ConnectionRegistrationStore;
        yield* registrationStore.register(bearerRegistration());
      }).pipe(Effect.provide(connectionStorageLayer));

      return Effect.gen(function* () {
        const identityFiber = yield* Effect.forkChild(acceptIdentity, { startImmediately: true });
        const registrationFiber = yield* Effect.forkChild(registerConnection, {
          startImmediately: true,
        });
        yield* Effect.promise(() => bothReads);
        releaseReads();
        yield* Fiber.join(identityFiber);
        yield* Fiber.join(registrationFiber);

        const document = decodeCatalog(storedCatalog);
        expect(document.acceptedStorageIdentities).toEqual([
          { targetKey: "platform:primary", storageInstanceId: "store-desktop" },
        ]);
        expect(document.targets).toEqual([bearerRegistration().target]);
        expect(document.profiles).toEqual([bearerRegistration().profile]);
        expect(document.credentials).toHaveLength(1);
        expect(compareAndSetConnectionCatalog).toHaveBeenCalledTimes(3);
      });
    },
  );

  it.effect("persists accepted identities through the desktop catalog bridge", () => {
    installFakeIndexedDb();
    let storedCatalog = encodeCatalog(emptyCatalog);
    const compareAndSetConnectionCatalog = vi.fn((expected: string | null, raw: string) => {
      if (storedCatalog !== expected) return Promise.resolve(false);
      storedCatalog = raw;
      return Promise.resolve(true);
    });
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn(() => Promise.resolve(storedCatalog)),
        compareAndSetConnectionCatalog,
      },
    });

    return Effect.gen(function* () {
      const identityStore = yield* AcceptedStorageIdentityStore;

      yield* identityStore.accept({
        targetKey: "platform:primary",
        storageInstanceId: "store-desktop",
      });

      expect(yield* identityStore.get("platform:primary")).toEqual(Option.some("store-desktop"));
      expect(decodeCatalog(storedCatalog).acceptedStorageIdentities).toEqual([
        {
          targetKey: "platform:primary",
          storageInstanceId: "store-desktop",
        },
      ]);
      expect(compareAndSetConnectionCatalog).toHaveBeenCalledTimes(1);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("persists an accepted identity without altering catalog connection data", () => {
    const handle = installFakeIndexedDb();
    return Effect.gen(function* () {
      const registrationStore = yield* ConnectionRegistrationStore;
      const tokenStore = yield* TokenStore.RemoteDpopAccessTokenStore;
      const identityStore = yield* AcceptedStorageIdentityStore;

      yield* registrationStore.register(bearerRegistration());
      yield* tokenStore.put(remoteToken);

      const rawBefore = handle.stores.get("catalog")?.get("document");
      expect(typeof rawBefore).toBe("string");
      const before = decodeCatalog(rawBefore as string);

      expect(Option.isNone(yield* identityStore.get("bearer:connection-1"))).toBe(true);
      yield* identityStore.accept({
        targetKey: "bearer:connection-1",
        storageInstanceId: "store-a",
      });

      expect(yield* identityStore.get("bearer:connection-1")).toEqual(Option.some("store-a"));
      const rawAfter = handle.stores.get("catalog")?.get("document");
      expect(typeof rawAfter).toBe("string");
      const after = decodeCatalog(rawAfter as string);
      expect(after).toEqual({
        ...before,
        acceptedStorageIdentities: [
          {
            targetKey: "bearer:connection-1",
            storageInstanceId: "store-a",
          },
        ],
      });
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("redacts identity persistence causes behind operation-specific errors", () => {
    installFakeIndexedDb();
    const leakedCause = "secret-token at /Users/alice/private/catalog.json";
    vi.stubGlobal("window", {
      desktopBridge: {
        getConnectionCatalog: vi.fn().mockRejectedValue(new Error(leakedCause)),
        setConnectionCatalog: vi.fn().mockRejectedValue(new Error(leakedCause)),
        compareAndSetConnectionCatalog: vi.fn().mockRejectedValue(new Error(leakedCause)),
      },
    });

    return Effect.gen(function* () {
      const identityStore = yield* AcceptedStorageIdentityStore;

      const loadError = yield* Effect.flip(identityStore.get("platform:primary"));
      expect(loadError.operation).toBe("load-storage-identity");
      expect(loadError.message).not.toContain("secret-token");
      expect(loadError.message).not.toContain("/Users/alice");

      const acceptError = yield* Effect.flip(
        identityStore.accept({
          targetKey: "platform:primary",
          storageInstanceId: "store-a",
        }),
      );
      expect(acceptError.operation).toBe("accept-storage-identity");
      expect(acceptError.message).not.toContain("secret-token");
      expect(acceptError.message).not.toContain("/Users/alice");
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("registers, reads, updates, and removes catalog-backed stores", () => {
    installFakeIndexedDb();
    return Effect.gen(function* () {
      const targetStore = yield* ConnectionTargetStore;
      const registrationStore = yield* ConnectionRegistrationStore;
      const profileStore = yield* ProfileStore.ConnectionProfileStore;
      const credentialStore = yield* CredentialStore.ConnectionCredentialStore;
      const tokenStore = yield* TokenStore.RemoteDpopAccessTokenStore;

      expect(yield* targetStore.list).toEqual([]);
      expect(Option.isNone(yield* profileStore.get(connectionId))).toBe(true);
      expect(Option.isNone(yield* credentialStore.get(connectionId))).toBe(true);
      expect(Option.isNone(yield* tokenStore.get(environmentId))).toBe(true);

      yield* registrationStore.register(bearerRegistration());

      const targets = yield* targetStore.list;
      expect(targets).toHaveLength(1);
      expect(targets[0]!.environmentId).toBe(environmentId);
      expect(Option.isSome(yield* profileStore.get(connectionId))).toBe(true);
      expect(Option.isSome(yield* credentialStore.get(connectionId))).toBe(true);

      // Remote DPoP token round-trip.
      yield* tokenStore.put(remoteToken);
      const token = yield* tokenStore.get(environmentId);
      expect(Option.isSome(token)).toBe(true);
      yield* tokenStore.remove(environmentId);
      expect(Option.isNone(yield* tokenStore.get(environmentId))).toBe(true);

      // Direct profile/credential mutation seams.
      yield* profileStore.put(bearerRegistration().profile);
      expect(Option.isSome(yield* profileStore.get(connectionId))).toBe(true);
      yield* profileStore.remove(connectionId);
      expect(Option.isNone(yield* profileStore.get(connectionId))).toBe(true);
      yield* credentialStore.put(connectionId, new BearerConnectionCredential({ token: "t2" }));
      expect(Option.isSome(yield* credentialStore.get(connectionId))).toBe(true);
      yield* credentialStore.remove(connectionId);
      expect(Option.isNone(yield* credentialStore.get(connectionId))).toBe(true);

      yield* registrationStore.remove(bearerRegistration().target);
      expect(yield* targetStore.list).toEqual([]);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("refreshes a credential only while its environment is saved", () => {
    installFakeIndexedDb();
    return Effect.gen(function* () {
      const registrationStore = yield* ConnectionRegistrationStore;
      const credentialStore = yield* CredentialStore.ConnectionCredentialStore;
      const refreshed = new BearerConnectionCredential({ token: "refreshed-token" });

      yield* registrationStore.register(bearerRegistration());
      expect(yield* credentialStore.putIfSaved(connectionId, refreshed)).toBe(true);
      expect(yield* credentialStore.get(connectionId)).toEqual(Option.some(refreshed));

      yield* registrationStore.remove(bearerRegistration().target);
      expect(yield* credentialStore.get(connectionId)).toEqual(Option.none());
      expect(yield* credentialStore.putIfSaved(connectionId, refreshed)).toBe(false);
      expect(yield* credentialStore.get(connectionId)).toEqual(Option.none());
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("preserves a replacement written by another registration store", () => {
    installFakeIndexedDb();
    const first = bearerRegistration();
    const replacement = new BearerConnectionRegistration({
      target: new BearerConnectionTarget({
        environmentId,
        label: "Replacement backend",
        connectionId,
        serverEnvironmentId: null,
      }),
      profile: new BearerConnectionProfile({
        connectionId,
        environmentId,
        label: "Replacement backend",
        httpBaseUrl: "https://replacement.example.test/",
        wsBaseUrl: "wss://replacement.example.test/",
        hostKey: "replacement-host-key",
      }),
      credential: new BearerConnectionCredential({ token: "replacement-token" }),
    });

    return Effect.gen(function* () {
      const firstRegistrationStore = yield* ConnectionRegistrationStore;
      yield* firstRegistrationStore.register(first);

      yield* Effect.gen(function* () {
        const secondRegistrationStore = yield* ConnectionRegistrationStore;
        const targetStore = yield* ConnectionTargetStore;
        const profileStore = yield* ProfileStore.ConnectionProfileStore;
        const credentialStore = yield* CredentialStore.ConnectionCredentialStore;

        yield* secondRegistrationStore.register(replacement);
        const removal = yield* firstRegistrationStore.removeIfMatching(first);

        expect(removal).toEqual({
          removed: false,
          current: { target: replacement.target, profile: Option.some(replacement.profile) },
        });
        expect(yield* targetStore.list).toEqual([replacement.target]);
        expect(yield* profileStore.get(connectionId)).toEqual(Option.some(replacement.profile));
        expect(yield* credentialStore.get(connectionId)).toEqual(
          Option.some(replacement.credential),
        );
      }).pipe(Effect.provide(connectionStorageLayer));
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("conditionally removes the exact durable registration", () => {
    installFakeIndexedDb();
    const registration = bearerRegistration();

    return Effect.gen(function* () {
      const registrationStore = yield* ConnectionRegistrationStore;
      const targetStore = yield* ConnectionTargetStore;
      const profileStore = yield* ProfileStore.ConnectionProfileStore;
      const credentialStore = yield* CredentialStore.ConnectionCredentialStore;

      yield* registrationStore.register(registration);
      expect(yield* registrationStore.removeIfMatching(registration)).toEqual({
        removed: true,
        current: null,
      });
      expect(yield* targetStore.list).toEqual([]);
      expect(yield* profileStore.get(connectionId)).toEqual(Option.none());
      expect(yield* credentialStore.get(connectionId)).toEqual(Option.none());
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("renames only the saved target label and reports an unsaved environment", () => {
    installFakeIndexedDb();
    const registration = bearerRegistration();

    return Effect.gen(function* () {
      const registrationStore = yield* ConnectionRegistrationStore;
      const targetStore = yield* ConnectionTargetStore;
      const profileStore = yield* ProfileStore.ConnectionProfileStore;
      const credentialStore = yield* CredentialStore.ConnectionCredentialStore;
      const tokenStore = yield* TokenStore.RemoteDpopAccessTokenStore;

      yield* registrationStore.register(registration);
      yield* tokenStore.put(remoteToken);

      const renamed = new BearerConnectionTarget({
        environmentId,
        label: "GPU box",
        connectionId,
        serverEnvironmentId: null,
      });
      expect(yield* registrationStore.relabel(environmentId, "GPU box")).toEqual(
        Option.some(renamed),
      );
      expect(yield* targetStore.list).toEqual([renamed]);
      expect(yield* profileStore.get(connectionId)).toEqual(Option.some(registration.profile));
      expect(yield* credentialStore.get(connectionId)).toEqual(
        Option.some(registration.credential),
      );
      expect(yield* tokenStore.get(environmentId)).toEqual(Option.some(remoteToken));

      expect(yield* registrationStore.relabel(otherEnvironmentId, "Elsewhere")).toEqual(
        Option.none(),
      );
      expect(yield* targetStore.list).toEqual([renamed]);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("persists and restores shell and thread snapshots", () => {
    installFakeIndexedDb();
    return Effect.gen(function* () {
      const cacheStore = yield* EnvironmentCacheStore;

      expect(Option.isNone(yield* cacheStore.loadShell(environmentId))).toBe(true);
      yield* cacheStore.saveShell(environmentId, shellSnapshot());
      const shell = yield* cacheStore.loadShell(environmentId);
      expect(Option.isSome(shell)).toBe(true);

      expect(Option.isNone(yield* cacheStore.loadThread(environmentId, threadId))).toBe(true);
      yield* cacheStore.saveThread(environmentId, orchestrationThread());
      const thread = yield* cacheStore.loadThread(environmentId, threadId);
      expect(Option.isSome(thread)).toBe(true);

      // A thread cached under a different environment is not returned.
      expect(Option.isNone(yield* cacheStore.loadThread(otherEnvironmentId, threadId))).toBe(true);

      yield* cacheStore.removeThread(environmentId, threadId);
      expect(Option.isNone(yield* cacheStore.loadThread(environmentId, threadId))).toBe(true);

      // Repopulate, then clear the whole environment (shell + thread range).
      yield* cacheStore.saveShell(environmentId, shellSnapshot());
      yield* cacheStore.saveThread(environmentId, orchestrationThread());
      yield* cacheStore.clear(environmentId);
      expect(Option.isNone(yield* cacheStore.loadShell(environmentId))).toBe(true);
      expect(Option.isNone(yield* cacheStore.loadThread(environmentId, threadId))).toBe(true);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("fails to build when IndexedDB is unavailable", () => {
    installFakeIndexedDb({ open: "undefined" });
    return ConnectionTargetStore.pipe(
      Effect.provide(connectionStorageLayer),
      Effect.flip,
      Effect.asVoid,
    );
  });

  it.effect("fails to build when the database open request errors", () => {
    installFakeIndexedDb({ open: "error" });
    return ConnectionTargetStore.pipe(
      Effect.provide(connectionStorageLayer),
      Effect.flip,
      Effect.asVoid,
    );
  });

  it.effect("maps catalog and cache read failures to operation-specific persistence errors", () => {
    installFakeIndexedDb({ fault: "get" });
    return Effect.gen(function* () {
      const targetStore = yield* ConnectionTargetStore;
      const cacheStore = yield* EnvironmentCacheStore;

      const targetsError = yield* Effect.flip(targetStore.list);
      expect(targetsError.operation).toBe("list-targets");
      const shellError = yield* Effect.flip(cacheStore.loadShell(environmentId));
      expect(shellError.operation).toBe("load-shell");
      const threadError = yield* Effect.flip(cacheStore.loadThread(environmentId, threadId));
      expect(threadError.operation).toBe("load-thread");
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect(
    "maps catalog and cache write failures to operation-specific persistence errors",
    () => {
      installFakeIndexedDb({ fault: "put" });
      return Effect.gen(function* () {
        const registrationStore = yield* ConnectionRegistrationStore;
        const cacheStore = yield* EnvironmentCacheStore;

        const registerError = yield* Effect.flip(registrationStore.register(bearerRegistration()));
        expect(registerError.operation).toBe("register-connection");
        const removeError = yield* Effect.flip(
          registrationStore.remove(bearerRegistration().target),
        );
        expect(removeError.operation).toBe("remove-connection");
        const shellError = yield* Effect.flip(cacheStore.saveShell(environmentId, shellSnapshot()));
        expect(shellError.operation).toBe("save-shell");
        const threadError = yield* Effect.flip(
          cacheStore.saveThread(environmentId, orchestrationThread()),
        );
        expect(threadError.operation).toBe("save-thread");
      }).pipe(Effect.provide(connectionStorageLayer));
    },
  );

  it.effect("rejects malformed cached snapshots and ignores snapshots scoped elsewhere", () => {
    const handle = installFakeIndexedDb();
    const shellStore = new Map<IDBValidKey, unknown>();
    const threadStore = new Map<IDBValidKey, unknown>();
    handle.stores.set("shell", shellStore);
    handle.stores.set("thread", threadStore);
    shellStore.set(environmentId, "{malformed");
    threadStore.set(`${environmentId}:${threadId}`, "{malformed");

    return Effect.gen(function* () {
      const cacheStore = yield* EnvironmentCacheStore;
      expect((yield* Effect.flip(cacheStore.loadShell(environmentId))).operation).toBe(
        "load-shell",
      );
      expect((yield* Effect.flip(cacheStore.loadThread(environmentId, threadId))).operation).toBe(
        "load-thread",
      );

      shellStore.set(
        environmentId,
        encodeUnknownJson({
          schemaVersion: 1,
          environmentId: otherEnvironmentId,
          snapshot: shellSnapshot(),
        }),
      );
      threadStore.set(
        `${environmentId}:${threadId}`,
        encodeUnknownJson({
          schemaVersion: 1,
          environmentId: otherEnvironmentId,
          threadId,
          thread: orchestrationThread(),
        }),
      );
      expect(Option.isNone(yield* cacheStore.loadShell(environmentId))).toBe(true);
      expect(Option.isNone(yield* cacheStore.loadThread(environmentId, threadId))).toBe(true);
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("maps malformed save payloads and removal failures to persistence errors", () => {
    installFakeIndexedDb();
    return Effect.gen(function* () {
      const cacheStore = yield* EnvironmentCacheStore;

      expect(
        (yield* Effect.flip(cacheStore.saveShell(environmentId, null as never))).operation,
      ).toBe("save-shell");
      expect(
        (yield* Effect.flip(cacheStore.saveThread(environmentId, { id: threadId } as never)))
          .operation,
      ).toBe("save-thread");
    }).pipe(Effect.provide(connectionStorageLayer));
  });

  it.effect("maps delete and cursor failures to the exact cache operations", () => {
    const deleteHandle = installFakeIndexedDb({ fault: "delete" });
    const deleteLayer = connectionStorageLayer;
    return Effect.gen(function* () {
      const deleteStore = yield* EnvironmentCacheStore.pipe(Effect.provide(deleteLayer));
      expect(
        (yield* Effect.flip(deleteStore.removeThread(environmentId, threadId))).operation,
      ).toBe("remove-thread");

      vi.unstubAllGlobals();
      installFakeIndexedDb({ fault: "cursor" });
      const cursorStore = yield* EnvironmentCacheStore.pipe(Effect.provide(connectionStorageLayer));
      expect((yield* Effect.flip(cursorStore.clear(environmentId))).operation).toBe(
        "clear-environment",
      );
      expect(deleteHandle.stores).toBeDefined();
    });
  });
});
