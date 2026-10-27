import {
  AcceptedStorageIdentityStore,
  type ConnectionCatalogHealth,
  ConnectionCatalogHealthStore,
  ConnectionCatalogDocument,
  type ConnectionCatalogDocument as ConnectionCatalogDocumentType,
  ConnectionPersistenceError,
  ConnectionRegistrationStore,
  ConnectionTargetStore,
  EMPTY_CONNECTION_CATALOG_DOCUMENT,
  EnvironmentCacheStore,
  registerConnectionInCatalog,
  relabelConnectionInCatalog,
  removeCatalogValue,
  removeConnectionFromCatalog,
  removeConnectionRegistrationFromCatalog,
  replaceCatalogValue,
  replaceSavedConnectionCredential,
} from "@coddy/client-runtime/platform";
import { TokenStore } from "@coddy/client-runtime/authorization";
import {
  ConnectionTransientError,
  CredentialStore,
  ProfileStore,
} from "@coddy/client-runtime/connection";
import {
  EnvironmentId,
  OrchestrationShellSnapshot,
  OrchestrationThread,
  ThreadId,
} from "@coddy/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";

import {
  CONNECTION_DATABASE_NAME,
  CONNECTION_DATABASE_VERSION,
  monitorConnectionDatabaseOpenRequest,
  reportConnectionDatabaseUnavailable,
} from "./databaseHealth";

const CATALOG_STORE_NAME = "catalog";
const SHELL_STORE_NAME = "shell";
const THREAD_STORE_NAME = "thread";
const CATALOG_KEY = "document";
const MAX_CATALOG_COMPARE_AND_SET_ATTEMPTS = 8;
const CORRUPT_CATALOG_MESSAGE =
  "The connection catalog is corrupt and must be reset before it can be changed.";
const SHELL_SNAPSHOT_CACHE_SCHEMA_VERSION = 1;

const StoredShellSnapshot = Schema.Struct({
  schemaVersion: Schema.Literal(SHELL_SNAPSHOT_CACHE_SCHEMA_VERSION),
  environmentId: EnvironmentId,
  snapshot: OrchestrationShellSnapshot,
});
const StoredShellSnapshotJson = Schema.fromJsonString(StoredShellSnapshot);
const StoredThreadSnapshot = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  environmentId: EnvironmentId,
  threadId: ThreadId,
  thread: OrchestrationThread,
});
const StoredThreadSnapshotJson = Schema.fromJsonString(StoredThreadSnapshot);
const ConnectionCatalogDocumentJson = Schema.fromJsonString(ConnectionCatalogDocument);
const decodeConnectionCatalogDocument = Schema.decodeUnknownEffect(ConnectionCatalogDocumentJson);
const encodeConnectionCatalogDocument = Schema.encodeEffect(ConnectionCatalogDocumentJson);
const decodeStoredShellSnapshot = Schema.decodeUnknownEffect(StoredShellSnapshotJson);
const encodeStoredShellSnapshot = Schema.encodeEffect(StoredShellSnapshotJson);
const decodeStoredThreadSnapshot = Schema.decodeUnknownEffect(StoredThreadSnapshotJson);
const encodeStoredThreadSnapshot = Schema.encodeEffect(StoredThreadSnapshotJson);

function catalogError(operation: string, cause: unknown) {
  return new ConnectionTransientError({
    reason: "remote-unavailable",
    detail: `Could not ${operation} the local connection catalog: ${String(cause)}`,
  });
}

function persistenceError(
  operation:
    | "list-targets"
    | "register-connection"
    | "remove-connection"
    | "rename-connection"
    | "load-shell"
    | "save-shell"
    | "load-thread"
    | "save-thread"
    | "remove-thread"
    | "clear-environment",
  cause: unknown,
) {
  return new ConnectionPersistenceError({
    operation,
    message: `Could not ${operation.replaceAll("-", " ")}: ${String(cause)}`,
  });
}

function storageIdentityPersistenceError(
  operation: "load-storage-identity" | "accept-storage-identity",
) {
  return new ConnectionPersistenceError({
    operation,
    message:
      operation === "load-storage-identity"
        ? "Could not load the accepted storage identity."
        : "Could not accept the reported storage identity.",
  });
}

function catalogResetPersistenceError() {
  return new ConnectionPersistenceError({
    operation: "reset-connection-catalog",
    message: "Could not reset the connection catalog.",
  });
}

const CONNECTION_DATABASE_BLOCKED_OPEN_TIMEOUT_MS = 10_000;

/**
 * Whether the connection catalog itself lives in IndexedDB. On protected
 * desktop hosts the catalog (saved servers, credentials, accepted storage
 * identities) is served from the native store, and an IndexedDB reset only
 * removes cached shell and thread state.
 */
export function connectionCatalogLivesInIndexedDb(): boolean {
  const bridge = typeof window === "undefined" ? undefined : window.desktopBridge;
  return (
    bridge?.getConnectionCatalog === undefined ||
    bridge.compareAndSetConnectionCatalog === undefined
  );
}

const openDatabase = Effect.fn("web.connectionStorage.openDatabase")(function* (
  databaseName = CONNECTION_DATABASE_NAME,
) {
  return yield* Effect.callback<IDBDatabase, ConnectionTransientError>((resume) => {
    if (typeof indexedDB === "undefined") {
      if (databaseName === CONNECTION_DATABASE_NAME) {
        reportConnectionDatabaseUnavailable("IndexedDB is unavailable in this browser context.");
      }
      resume(
        Effect.fail(catalogError("open", "IndexedDB is unavailable in this browser context.")),
      );
      return;
    }
    const request = indexedDB.open(databaseName, CONNECTION_DATABASE_VERSION);
    if (databaseName === CONNECTION_DATABASE_NAME) {
      monitorConnectionDatabaseOpenRequest(request);
    }
    // A blocked open (another tab holds an older version) may still complete
    // once the blocker closes, but it must not stall the storage layer
    // forever: fail after a bounded wait so the fault surfaces.
    let blockedTimer: ReturnType<typeof setTimeout> | undefined;
    const clearBlockedTimer = () => {
      if (blockedTimer !== undefined) {
        clearTimeout(blockedTimer);
        blockedTimer = undefined;
      }
    };
    request.addEventListener("blocked", () => {
      if (blockedTimer !== undefined) return;
      blockedTimer = setTimeout(() => {
        resume(
          Effect.fail(
            catalogError(
              "open",
              "Another tab or process kept an older connection database open past the wait deadline.",
            ),
          ),
        );
      }, CONNECTION_DATABASE_BLOCKED_OPEN_TIMEOUT_MS);
    });
    request.addEventListener("upgradeneeded", () => {
      if (!request.result.objectStoreNames.contains(CATALOG_STORE_NAME)) {
        request.result.createObjectStore(CATALOG_STORE_NAME);
      }
      if (!request.result.objectStoreNames.contains(SHELL_STORE_NAME)) {
        request.result.createObjectStore(SHELL_STORE_NAME);
      }
      if (!request.result.objectStoreNames.contains(THREAD_STORE_NAME)) {
        request.result.createObjectStore(THREAD_STORE_NAME);
      }
    });
    request.addEventListener("error", () => {
      clearBlockedTimer();
      resume(Effect.fail(catalogError("open", request.error ?? "Unknown IndexedDB error")));
    });
    request.addEventListener("success", () => {
      clearBlockedTimer();
      const database = request.result;
      // Close this connection when another context upgrades or deletes the
      // database, so one tab can never hold every other tab's recovery
      // hostage; later operations surface as transient errors.
      database.addEventListener("versionchange", () => {
        database.close();
      });
      resume(Effect.succeed(database));
    });
  });
});

function readDatabaseValue(database: IDBDatabase, storeName: string, key: IDBValidKey) {
  return Effect.callback<unknown, ConnectionTransientError>((resume) => {
    const request = database.transaction(storeName, "readonly").objectStore(storeName).get(key);
    request.addEventListener("error", () => {
      resume(Effect.fail(catalogError("read", request.error ?? "Unknown IndexedDB read error")));
    });
    request.addEventListener("success", () => {
      resume(Effect.succeed(request.result));
    });
  }).pipe(Effect.withSpan("web.connectionStorage.readDatabaseValue"));
}

function writeDatabaseValue(
  database: IDBDatabase,
  storeName: string,
  key: IDBValidKey,
  value: unknown,
) {
  return Effect.callback<void, ConnectionTransientError>((resume) => {
    const transaction = database.transaction(storeName, "readwrite");
    transaction.addEventListener("error", () => {
      resume(
        Effect.fail(catalogError("write", transaction.error ?? "Unknown IndexedDB write error")),
      );
    });
    transaction.addEventListener("complete", () => {
      resume(Effect.void);
    });
    transaction.objectStore(storeName).put(value, key);
  }).pipe(Effect.withSpan("web.connectionStorage.writeDatabaseValue"));
}

function compareAndSetDatabaseValue(
  database: IDBDatabase,
  storeName: string,
  key: IDBValidKey,
  expected: string | null,
  next: string,
) {
  return Effect.callback<boolean, ConnectionTransientError>((resume) => {
    const transaction = database.transaction(storeName, "readwrite");
    let matched = false;
    transaction.addEventListener("error", () => {
      resume(
        Effect.fail(
          catalogError(
            "compare and set",
            transaction.error ?? "Unknown IndexedDB transaction error",
          ),
        ),
      );
    });
    transaction.addEventListener("abort", () => {
      resume(
        Effect.fail(
          catalogError(
            "compare and set",
            transaction.error ?? "Unknown IndexedDB transaction abort",
          ),
        ),
      );
    });
    transaction.addEventListener("complete", () => {
      resume(Effect.succeed(matched));
    });

    const store = transaction.objectStore(storeName);
    const request = store.get(key);
    request.addEventListener("success", () => {
      const current = typeof request.result === "string" ? request.result : null;
      if (current !== expected) return;
      matched = true;
      store.put(next, key);
    });
  }).pipe(Effect.withSpan("web.connectionStorage.compareAndSetDatabaseValue"));
}

function compareDatabaseValue(
  database: IDBDatabase,
  storeName: string,
  key: IDBValidKey,
  expected: string | null,
) {
  return Effect.callback<boolean, ConnectionTransientError>((resume) => {
    const transaction = database.transaction(storeName, "readwrite");
    let matched = false;
    transaction.addEventListener("error", () => {
      resume(
        Effect.fail(
          catalogError("compare", transaction.error ?? "Unknown IndexedDB transaction error"),
        ),
      );
    });
    transaction.addEventListener("abort", () => {
      resume(
        Effect.fail(
          catalogError("compare", transaction.error ?? "Unknown IndexedDB transaction abort"),
        ),
      );
    });
    transaction.addEventListener("complete", () => {
      resume(Effect.succeed(matched));
    });

    const request = transaction.objectStore(storeName).get(key);
    request.addEventListener("success", () => {
      const current = typeof request.result === "string" ? request.result : null;
      matched = current === expected;
    });
  }).pipe(Effect.withSpan("web.connectionStorage.compareDatabaseValue"));
}

function removeDatabaseValue(database: IDBDatabase, storeName: string, key: IDBValidKey) {
  return Effect.callback<void, ConnectionTransientError>((resume) => {
    const transaction = database.transaction(storeName, "readwrite");
    transaction.addEventListener("error", () => {
      resume(
        Effect.fail(catalogError("remove", transaction.error ?? "Unknown IndexedDB remove error")),
      );
    });
    transaction.addEventListener("complete", () => {
      resume(Effect.void);
    });
    transaction.objectStore(storeName).delete(key);
  }).pipe(Effect.withSpan("web.connectionStorage.removeDatabaseValue"));
}

function removeDatabaseValuesInRange(database: IDBDatabase, storeName: string, range: IDBKeyRange) {
  return Effect.callback<void, ConnectionTransientError>((resume) => {
    const transaction = database.transaction(storeName, "readwrite");
    transaction.addEventListener("error", () => {
      resume(
        Effect.fail(catalogError("remove", transaction.error ?? "Unknown IndexedDB cursor error")),
      );
    });
    transaction.addEventListener("complete", () => {
      resume(Effect.void);
    });
    const request = transaction.objectStore(storeName).openCursor(range);
    request.addEventListener("error", () => {
      resume(
        Effect.fail(catalogError("remove", request.error ?? "Unknown IndexedDB cursor error")),
      );
    });
    request.addEventListener("success", () => {
      const cursor = request.result;
      if (cursor === null) {
        return;
      }
      cursor.delete();
      cursor.continue();
    });
  }).pipe(Effect.withSpan("web.connectionStorage.removeDatabaseValuesInRange"));
}

function threadCacheKey(environmentId: EnvironmentId, threadId: ThreadId) {
  return `${environmentId}:${threadId}`;
}

const decodeCatalog = Effect.fn("web.connectionStorage.decodeCatalog")(function* (raw: string) {
  return yield* decodeConnectionCatalogDocument(raw).pipe(
    Effect.mapError((cause) => catalogError("decode", cause)),
  );
});

const encodeCatalog = Effect.fn("web.connectionStorage.encodeCatalog")(function* (
  catalog: ConnectionCatalogDocumentType,
) {
  return yield* encodeConnectionCatalogDocument(catalog).pipe(
    Effect.mapError((cause) => catalogError("encode", cause)),
  );
});

export interface CatalogBackend {
  readonly read: Effect.Effect<string | null, ConnectionTransientError>;
  readonly compare: (expected: string | null) => Effect.Effect<boolean, ConnectionTransientError>;
  readonly compareAndSet: (
    expected: string | null,
    next: string,
  ) => Effect.Effect<boolean, ConnectionTransientError>;
  readonly quarantine?: (raw: string) => Effect.Effect<void, ConnectionTransientError>;
}

export function makeCatalogBackend(database: IDBDatabase): CatalogBackend {
  const bridge = window.desktopBridge;
  if (
    bridge?.getConnectionCatalog !== undefined &&
    bridge.compareAndSetConnectionCatalog !== undefined
  ) {
    return {
      read: Effect.tryPromise({
        try: () => bridge.getConnectionCatalog!(),
        catch: (cause) => catalogError("load", cause),
      }),
      compare: (expected) => {
        if (bridge.compareConnectionCatalog === undefined) {
          return Effect.fail(
            catalogError("compare", "Desktop connection catalog comparison is unavailable."),
          );
        }
        return Effect.tryPromise({
          try: () => bridge.compareConnectionCatalog!(expected),
          catch: (cause) => catalogError("compare", cause),
        });
      },
      compareAndSet: (expected, next) => {
        if (bridge.compareAndSetConnectionCatalog === undefined) {
          return Effect.fail(
            catalogError(
              "compare and set",
              "Desktop connection catalog compare-and-set is unavailable.",
            ),
          );
        }
        return Effect.tryPromise({
          try: () => bridge.compareAndSetConnectionCatalog!(expected, next),
          catch: (cause) => catalogError("compare and set", cause),
        });
      },
    };
  }

  return {
    read: readDatabaseValue(database, CATALOG_STORE_NAME, CATALOG_KEY).pipe(
      Effect.map((value) => (typeof value === "string" ? value : null)),
    ),
    compare: (expected) =>
      compareDatabaseValue(database, CATALOG_STORE_NAME, CATALOG_KEY, expected),
    compareAndSet: (expected, next) =>
      compareAndSetDatabaseValue(database, CATALOG_STORE_NAME, CATALOG_KEY, expected, next),
    quarantine: (raw) =>
      writeDatabaseValue(database, CATALOG_STORE_NAME, `${CATALOG_KEY}:corrupt:${Date.now()}`, raw),
  };
}

const migrateLegacyRendererCatalog = Effect.fn(
  "web.connectionStorage.migrateLegacyRendererCatalog",
)(function* (backend: CatalogBackend) {
  const bridge = window.desktopBridge;
  if (
    bridge?.getConnectionCatalog === undefined ||
    bridge.compareAndSetConnectionCatalog !== undefined
  ) {
    return;
  }

  const raw = yield* Effect.tryPromise({
    try: () => bridge.getConnectionCatalog!(),
    catch: () => catalogError("migrate", "Could not read the legacy desktop catalog."),
  });
  if (raw === null) return;
  if (bridge.clearConnectionCatalog === undefined) {
    return yield* catalogError(
      "migrate",
      "Could not clear the legacy desktop catalog after migration.",
    );
  }

  const legacyDocument =
    raw.trim() === ""
      ? Option.none<ConnectionCatalogDocumentType>()
      : yield* decodeCatalog(raw).pipe(
          Effect.map(Option.some),
          Effect.orElseSucceed(() => Option.none<ConnectionCatalogDocumentType>()),
        );
  const clearLegacyCatalog = Effect.tryPromise({
    try: () => bridge.clearConnectionCatalog!(),
    catch: () => catalogError("migrate", "Could not clear the legacy desktop catalog."),
  });

  yield* Effect.uninterruptible(
    Effect.gen(function* () {
      for (let attempt = 0; attempt < MAX_CATALOG_COMPARE_AND_SET_ATTEMPTS; attempt += 1) {
        const current = yield* backend.read;
        if (current !== null && current.trim() !== "") {
          const currentDocument = yield* decodeCatalog(current).pipe(
            Effect.map(Option.some),
            Effect.orElseSucceed(() => Option.none<ConnectionCatalogDocumentType>()),
          );
          if (Option.isSome(currentDocument) || Option.isNone(legacyDocument)) {
            return yield* clearLegacyCatalog;
          }
        } else if (Option.isNone(legacyDocument)) {
          return yield* clearLegacyCatalog;
        }

        const migrated = yield* backend.compareAndSet(current, raw);
        if (migrated) {
          if (current !== null && current.trim() !== "" && backend.quarantine !== undefined) {
            yield* backend.quarantine(current).pipe(
              Effect.catch((cause) =>
                Effect.logWarning("Could not quarantine the replaced web connection catalog.", {
                  error: cause.message,
                }),
              ),
            );
          }
          return yield* clearLegacyCatalog;
        }
        yield* Effect.yieldNow;
      }

      return yield* catalogError(
        "migrate",
        "The browser catalog changed too many times during legacy desktop migration.",
      );
    }),
  );
});

interface CatalogStore {
  readonly health: SubscriptionRef.SubscriptionRef<ConnectionCatalogHealth>;
  readonly read: Effect.Effect<ConnectionCatalogDocumentType, ConnectionTransientError>;
  readonly reset: Effect.Effect<void, ConnectionTransientError>;
  readonly update: (
    transform: (catalog: ConnectionCatalogDocumentType) => ConnectionCatalogDocumentType,
  ) => Effect.Effect<void, ConnectionTransientError>;
  readonly modify: <A>(
    transform: (catalog: ConnectionCatalogDocumentType) => {
      readonly mutation:
        | { readonly _tag: "Keep" }
        | { readonly _tag: "Set"; readonly document: ConnectionCatalogDocumentType };
      readonly result: A;
    },
  ) => Effect.Effect<A, ConnectionTransientError>;
}

export const makeCatalogStore = Effect.fn("web.connectionStorage.makeCatalogStore")(function* (
  backend: CatalogBackend,
) {
  const health = yield* SubscriptionRef.make<ConnectionCatalogHealth>({ status: "ready" });
  const quarantinedRevision = yield* Ref.make<string | null>(null);

  const enterRecovery = Effect.fn("web.connectionStorage.enterCatalogRecovery")(function* (
    raw: string,
  ) {
    const priorRevision = yield* Ref.get(quarantinedRevision);
    if (priorRevision !== raw && backend.quarantine !== undefined) {
      yield* backend
        .quarantine(raw)
        .pipe(
          Effect.catch(() =>
            Effect.logWarning("Could not quarantine the corrupt web connection catalog."),
          ),
        );
    }
    yield* Ref.set(quarantinedRevision, raw);
    yield* SubscriptionRef.set(health, {
      status: "recovery-required",
      message: CORRUPT_CATALOG_MESSAGE,
    });
  });

  const leaveRecovery = Effect.gen(function* () {
    yield* Ref.set(quarantinedRevision, null);
    yield* SubscriptionRef.set(health, { status: "ready" });
  });

  const loadVersion = Effect.fn("web.connectionStorage.loadCatalog")(function* () {
    const raw = yield* backend.read;
    const currentHealth = yield* SubscriptionRef.get(health);
    if (raw === null || raw.trim() === "") {
      return {
        raw,
        document: EMPTY_CONNECTION_CATALOG_DOCUMENT,
        recoveryRequired: currentHealth.status === "recovery-required",
      } as const;
    }

    const decoded = yield* decodeCatalog(raw).pipe(
      Effect.map(Option.some),
      Effect.catch(() =>
        Effect.logWarning("The web connection catalog requires recovery.").pipe(
          Effect.as(Option.none<ConnectionCatalogDocumentType>()),
        ),
      ),
    );
    if (Option.isSome(decoded)) {
      if (currentHealth.status === "recovery-required") {
        yield* leaveRecovery;
      }
      return { raw, document: decoded.value, recoveryRequired: false } as const;
    }

    yield* enterRecovery(raw);
    return {
      raw,
      document: EMPTY_CONNECTION_CATALOG_DOCUMENT,
      recoveryRequired: true,
    } as const;
  });

  const read = loadVersion().pipe(Effect.map(({ document }) => document));
  const modify: CatalogStore["modify"] = Effect.fn("web.connectionStorage.modifyCatalog")(
    function* (transform) {
      for (let attempt = 0; attempt < MAX_CATALOG_COMPARE_AND_SET_ATTEMPTS; attempt += 1) {
        const { raw, document, recoveryRequired } = yield* loadVersion();
        if (recoveryRequired) {
          return yield* catalogError("update", CORRUPT_CATALOG_MESSAGE);
        }
        const transition = transform(document);
        const updated =
          transition.mutation._tag === "Keep"
            ? yield* Effect.uninterruptible(backend.compare(raw))
            : yield* Effect.uninterruptible(
                encodeCatalog(transition.mutation.document).pipe(
                  Effect.flatMap((next) => backend.compareAndSet(raw, next)),
                ),
              );
        if (updated) return transition.result;
        yield* Effect.yieldNow;
      }
      return yield* catalogError("update", "The connection catalog changed too many times.");
    },
  );
  const update: CatalogStore["update"] = (transform) =>
    modify((document) => ({
      mutation: { _tag: "Set", document: transform(document) },
      result: undefined,
    }));

  const reset = Effect.uninterruptible(
    Effect.gen(function* () {
      const emptyRaw = yield* encodeCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT);
      for (let attempt = 0; attempt < MAX_CATALOG_COMPARE_AND_SET_ATTEMPTS; attempt += 1) {
        const raw = yield* backend.read;
        if (raw !== null && raw.trim() !== "") {
          const decoded = yield* decodeCatalog(raw).pipe(Effect.option);
          if (Option.isSome(decoded)) {
            yield* leaveRecovery;
            return;
          }
          yield* enterRecovery(raw);
        }

        if (yield* backend.compareAndSet(raw, emptyRaw)) {
          yield* leaveRecovery;
          return;
        }
        yield* Effect.yieldNow;
      }
      return yield* catalogError("reset", "The connection catalog changed too many times.");
    }),
  ).pipe(Effect.withSpan("web.connectionStorage.resetCatalog"));

  return { health, modify, read, reset, update } satisfies CatalogStore;
});

export const connectionStorageLayer = Layer.effectContext(
  Effect.gen(function* () {
    const database = yield* Effect.acquireRelease(openDatabase(), (database) =>
      Effect.sync(() => database.close()),
    );
    const backend = makeCatalogBackend(database);
    yield* migrateLegacyRendererCatalog(backend);
    const catalog = yield* makeCatalogStore(backend);

    const catalogHealthStore = ConnectionCatalogHealthStore.of({
      state: catalog.health,
      reset: catalog.reset.pipe(Effect.mapError(catalogResetPersistenceError)),
    });

    const targetStore = ConnectionTargetStore.of({
      list: catalog.read.pipe(
        Effect.map((document) => document.targets),
        Effect.mapError((cause) => persistenceError("list-targets", cause)),
      ),
    });
    const registrationStore = ConnectionRegistrationStore.of({
      register: (registration) =>
        catalog
          .update((document) => registerConnectionInCatalog(document, registration))
          .pipe(Effect.mapError((cause) => persistenceError("register-connection", cause))),
      removeIfMatching: (registration) =>
        catalog
          .modify((document) => {
            const removal = removeConnectionRegistrationFromCatalog(document, registration);
            return removal.removed
              ? {
                  mutation: { _tag: "Set", document: removal.document },
                  result: { removed: true, current: null },
                }
              : {
                  mutation: { _tag: "Keep" },
                  result: { removed: false, current: removal.current },
                };
          })
          .pipe(Effect.mapError((cause) => persistenceError("remove-connection", cause))),
      remove: (target) =>
        catalog
          .update((document) => removeConnectionFromCatalog(document, target))
          .pipe(Effect.mapError((cause) => persistenceError("remove-connection", cause))),
      relabel: (environmentId, label) =>
        catalog
          .modify((document) => {
            const renamed = relabelConnectionInCatalog(document, environmentId, label);
            return renamed === null
              ? { mutation: { _tag: "Keep" }, result: Option.none() }
              : {
                  mutation: { _tag: "Set", document: renamed.document },
                  result: Option.some(renamed.target),
                };
          })
          .pipe(Effect.mapError((cause) => persistenceError("rename-connection", cause))),
    });
    const profileStore = ProfileStore.make({
      get: (connectionId) =>
        catalog.read.pipe(
          Effect.map((document) =>
            Option.fromUndefinedOr(
              document.profiles.find((profile) => profile.connectionId === connectionId),
            ),
          ),
        ),
      put: (profile) =>
        catalog.update((document) => ({
          ...document,
          profiles: replaceCatalogValue(document.profiles, (value) => value.connectionId, profile),
        })),
      remove: (connectionId) =>
        catalog.update((document) => ({
          ...document,
          profiles: removeCatalogValue(
            document.profiles,
            (value) => value.connectionId,
            connectionId,
          ),
        })),
    });
    const credentialStore = CredentialStore.make({
      get: (connectionId) =>
        catalog.read.pipe(
          Effect.map((document) =>
            Option.fromUndefinedOr(
              document.credentials.find((entry) => entry.connectionId === connectionId)?.credential,
            ),
          ),
        ),
      put: (connectionId, credential) =>
        catalog.update((document) => ({
          ...document,
          credentials: replaceCatalogValue(document.credentials, (value) => value.connectionId, {
            connectionId,
            credential,
          }),
        })),
      remove: (connectionId) =>
        catalog.update((document) => ({
          ...document,
          credentials: removeCatalogValue(
            document.credentials,
            (value) => value.connectionId,
            connectionId,
          ),
        })),
      putIfSaved: (connectionId, credential) =>
        catalog.modify((document) => {
          const next = replaceSavedConnectionCredential(document, connectionId, credential);
          return next === null
            ? { mutation: { _tag: "Keep" }, result: false }
            : { mutation: { _tag: "Set", document: next }, result: true };
        }),
    });
    const remoteTokenStore = TokenStore.make({
      get: (environmentId) =>
        catalog.read.pipe(
          Effect.map((document) =>
            Option.fromUndefinedOr(
              document.remoteDpopTokens.find((token) => token.environmentId === environmentId),
            ),
          ),
        ),
      put: (token) =>
        catalog.update((document) => ({
          ...document,
          remoteDpopTokens: replaceCatalogValue(
            document.remoteDpopTokens,
            (value) => value.environmentId,
            token,
          ),
        })),
      remove: (environmentId) =>
        catalog.update((document) => ({
          ...document,
          remoteDpopTokens: removeCatalogValue(
            document.remoteDpopTokens,
            (value) => value.environmentId,
            environmentId,
          ),
        })),
    });
    const acceptedStorageIdentityStore = AcceptedStorageIdentityStore.of({
      get: (targetKey) =>
        catalog.read.pipe(
          Effect.map((document) =>
            Option.fromUndefinedOr(
              document.acceptedStorageIdentities.find(
                (identity) => identity.targetKey === targetKey,
              )?.storageInstanceId,
            ),
          ),
          Effect.mapError(() => storageIdentityPersistenceError("load-storage-identity")),
        ),
      accept: (identity) =>
        catalog
          .update((document) => ({
            ...document,
            acceptedStorageIdentities: replaceCatalogValue(
              document.acceptedStorageIdentities,
              (value) => value.targetKey,
              identity,
            ),
          }))
          .pipe(Effect.mapError(() => storageIdentityPersistenceError("accept-storage-identity"))),
      rollbackAcceptance: (identity, previousStorageInstanceId) =>
        catalog
          .modify((document) => {
            const matches = document.acceptedStorageIdentities.some(
              (candidate) =>
                candidate.targetKey === identity.targetKey &&
                candidate.storageInstanceId === identity.storageInstanceId,
            );
            if (!matches) {
              return { mutation: { _tag: "Keep" }, result: false } as const;
            }
            const withoutAttempt = removeCatalogValue(
              document.acceptedStorageIdentities,
              (candidate) => candidate.targetKey,
              identity.targetKey,
            );
            return {
              mutation: {
                _tag: "Set",
                document: {
                  ...document,
                  acceptedStorageIdentities:
                    previousStorageInstanceId === null
                      ? withoutAttempt
                      : replaceCatalogValue(withoutAttempt, (candidate) => candidate.targetKey, {
                          targetKey: identity.targetKey,
                          storageInstanceId: previousStorageInstanceId,
                        }),
                },
              },
              result: true,
            } as const;
          })
          .pipe(Effect.mapError(() => storageIdentityPersistenceError("accept-storage-identity"))),
      transition: (targetKey, decide) =>
        catalog
          .modify((document) => {
            const acceptedStorageInstanceId =
              document.acceptedStorageIdentities.find(
                (identity) => identity.targetKey === targetKey,
              )?.storageInstanceId ?? null;
            const transition = decide(acceptedStorageInstanceId);
            if (transition.mutation._tag === "Keep") {
              return { mutation: { _tag: "Keep" }, result: transition.result };
            }
            return {
              mutation: {
                _tag: "Set",
                document: {
                  ...document,
                  acceptedStorageIdentities: replaceCatalogValue(
                    document.acceptedStorageIdentities,
                    (value) => value.targetKey,
                    {
                      targetKey,
                      storageInstanceId: transition.mutation.storageInstanceId,
                    },
                  ),
                },
              },
              result: transition.result,
            };
          })
          .pipe(Effect.mapError(() => storageIdentityPersistenceError("accept-storage-identity"))),
    });
    const cacheStore = EnvironmentCacheStore.of({
      loadShell: (environmentId) =>
        readDatabaseValue(database, SHELL_STORE_NAME, environmentId).pipe(
          Effect.flatMap((raw) => {
            if (typeof raw !== "string") {
              return Effect.succeed(Option.none());
            }
            return decodeStoredShellSnapshot(raw).pipe(
              Effect.mapError((cause) => persistenceError("load-shell", cause)),
              Effect.map((stored) =>
                stored.environmentId === environmentId
                  ? Option.some(stored.snapshot)
                  : Option.none(),
              ),
            );
          }),
          Effect.mapError((cause) =>
            cause._tag === "ConnectionPersistenceError"
              ? cause
              : persistenceError("load-shell", cause),
          ),
        ),
      saveShell: (environmentId, snapshot) =>
        Effect.gen(function* () {
          const encoded = yield* encodeStoredShellSnapshot({
            schemaVersion: SHELL_SNAPSHOT_CACHE_SCHEMA_VERSION,
            environmentId,
            snapshot,
          }).pipe(Effect.mapError((cause) => persistenceError("save-shell", cause)));
          yield* writeDatabaseValue(database, SHELL_STORE_NAME, environmentId, encoded);
        }).pipe(
          Effect.mapError((cause) =>
            cause._tag === "ConnectionPersistenceError"
              ? cause
              : persistenceError("save-shell", cause),
          ),
        ),
      loadThread: (environmentId, threadId) =>
        readDatabaseValue(
          database,
          THREAD_STORE_NAME,
          threadCacheKey(environmentId, threadId),
        ).pipe(
          Effect.flatMap((raw) => {
            if (typeof raw !== "string") {
              return Effect.succeed(Option.none());
            }
            return decodeStoredThreadSnapshot(raw).pipe(
              Effect.mapError((cause) => persistenceError("load-thread", cause)),
              Effect.map((stored) =>
                stored.environmentId === environmentId && stored.threadId === threadId
                  ? Option.some(stored.thread)
                  : Option.none(),
              ),
            );
          }),
          Effect.mapError((cause) =>
            cause._tag === "ConnectionPersistenceError"
              ? cause
              : persistenceError("load-thread", cause),
          ),
        ),
      saveThread: (environmentId, thread) =>
        Effect.gen(function* () {
          const encoded = yield* encodeStoredThreadSnapshot({
            schemaVersion: 1,
            environmentId,
            threadId: thread.id,
            thread,
          }).pipe(Effect.mapError((cause) => persistenceError("save-thread", cause)));
          yield* writeDatabaseValue(
            database,
            THREAD_STORE_NAME,
            threadCacheKey(environmentId, thread.id),
            encoded,
          );
        }).pipe(
          Effect.mapError((cause) =>
            cause._tag === "ConnectionPersistenceError"
              ? cause
              : persistenceError("save-thread", cause),
          ),
        ),
      removeThread: (environmentId, threadId) =>
        removeDatabaseValue(
          database,
          THREAD_STORE_NAME,
          threadCacheKey(environmentId, threadId),
        ).pipe(Effect.mapError((cause) => persistenceError("remove-thread", cause))),
      clear: (environmentId) =>
        Effect.all(
          [
            removeDatabaseValue(database, SHELL_STORE_NAME, environmentId),
            removeDatabaseValuesInRange(
              database,
              THREAD_STORE_NAME,
              IDBKeyRange.bound(`${environmentId}:`, `${environmentId}:\uffff`),
            ),
          ],
          { concurrency: "unbounded", discard: true },
        ).pipe(Effect.mapError((cause) => persistenceError("clear-environment", cause))),
    });

    return Context.make(ConnectionTargetStore, targetStore).pipe(
      Context.add(ConnectionRegistrationStore, registrationStore),
      Context.add(ProfileStore.ConnectionProfileStore, profileStore),
      Context.add(CredentialStore.ConnectionCredentialStore, credentialStore),
      Context.add(TokenStore.RemoteDpopAccessTokenStore, remoteTokenStore),
      Context.add(AcceptedStorageIdentityStore, acceptedStorageIdentityStore),
      Context.add(ConnectionCatalogHealthStore, catalogHealthStore),
      Context.add(EnvironmentCacheStore, cacheStore),
    );
  }),
);
