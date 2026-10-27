import { verifyDpopProof } from "@coddy/shared/dpop";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import { decodeJwt } from "jose";
import { afterEach, vi } from "vite-plus/test";

import {
  BrowserDpopError,
  browserCryptoLayer,
  createBrowserDpopProof,
  generateBrowserDpopKey,
  readStoredBrowserDpopKey,
  writeStoredBrowserDpopKey,
  type BrowserDpopKey,
} from "./dpop";

class FakeRequest {
  result: unknown = undefined;
  error: unknown = null;
  private readonly listeners = new Map<string, Array<() => void>>();

  addEventListener(type: string, handler: () => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), handler]);
  }

  fire(type: string): void {
    for (const handler of this.listeners.get(type) ?? []) {
      handler();
    }
  }
}

type IndexedDbFault = "none" | "open" | "read" | "write";

function installFakeIndexedDb(
  options: {
    readonly fault?: IndexedDbFault;
    readonly initial?: BrowserDpopKey;
    readonly existingStore?: boolean;
  } = {},
) {
  const values = new Map<IDBValidKey, unknown>();
  if (options.initial) {
    values.set("relay-dpop-proof-key", options.initial);
  }
  const hasStore = new Map<string, boolean>();
  let closed = false;
  const databaseFor = (name: string) => {
    const databaseValues = values;
    return {
      objectStoreNames: {
        contains: () => hasStore.get(name) ?? options.existingStore ?? false,
      },
      createObjectStore: () => {
        hasStore.set(name, true);
        return {};
      },
      transaction: () => {
        const transaction = new FakeRequest();
        return Object.assign(transaction, {
          objectStore: () => ({
            get: (key: IDBValidKey) => {
              const request = new FakeRequest();
              queueMicrotask(() => {
                if (options.fault === "read") {
                  request.error = new Error("read-denied");
                  request.fire("error");
                  return;
                }
                request.result = databaseValues.get(key);
                request.fire("success");
              });
              return request;
            },
            put: (value: unknown, key: IDBValidKey) => {
              queueMicrotask(() => {
                if (options.fault === "write") {
                  transaction.error = new Error("write-denied");
                  transaction.fire("error");
                  return;
                }
                databaseValues.set(key, value);
                transaction.fire("complete");
              });
            },
          }),
        });
      },
      close: () => {
        closed = true;
      },
    } as unknown as IDBDatabase;
  };
  vi.stubGlobal("indexedDB", {
    open: (name: string) => {
      const request = new FakeRequest();
      queueMicrotask(() => {
        if (options.fault === "open") {
          request.error = new Error("open-denied");
          request.fire("error");
          return;
        }
        request.result = databaseFor(name);
        request.fire("upgradeneeded");
        request.fire("success");
      });
      return request;
    },
  });
  return {
    values,
    isClosed: () => closed,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("browser DPoP proofs", () => {
  it.effect("signs relay resource proofs with an access-token hash", () =>
    Effect.gen(function* () {
      vi.stubGlobal("indexedDB", undefined);
      const proofKey = yield* generateBrowserDpopKey;
      const proof = yield* createBrowserDpopProof({
        method: "POST",
        url: "https://relay.example.test/v1/environments/env-1/connect?ignored=true",
        accessToken: "relay-access-token",
        proofKey,
      }).pipe(Effect.provide(browserCryptoLayer));
      const issuedAt = decodeJwt(proof.proof).iat;
      expect(issuedAt).toBeTypeOf("number");

      expect(
        verifyDpopProof({
          proof: proof.proof,
          method: "POST",
          url: "https://relay.example.test/v1/environments/env-1/connect",
          expectedThumbprint: proof.thumbprint,
          expectedAccessToken: "relay-access-token",
          nowEpochSeconds: issuedAt!,
        }),
      ).toMatchObject({ ok: true });
    }),
  );

  it.effect("skips persistent storage when IndexedDB is unavailable", () =>
    Effect.gen(function* () {
      vi.stubGlobal("indexedDB", undefined);
      const proofKey = yield* generateBrowserDpopKey;

      expect(yield* readStoredBrowserDpopKey()).toBeNull();
      yield* writeStoredBrowserDpopKey(proofKey);
    }),
  );

  it.effect("creates the key store and round-trips a proof key", () =>
    Effect.gen(function* () {
      const storage = installFakeIndexedDb();
      const proofKey = yield* generateBrowserDpopKey;

      expect(yield* readStoredBrowserDpopKey()).toBeNull();
      yield* writeStoredBrowserDpopKey(proofKey);
      expect(yield* readStoredBrowserDpopKey()).toEqual(proofKey);
      expect(storage.isClosed()).toBe(true);
    }),
  );

  it.effect("keeps an existing key store intact", () =>
    Effect.gen(function* () {
      const proofKey = yield* generateBrowserDpopKey;
      installFakeIndexedDb({ initial: proofKey, existingStore: true });

      expect(yield* readStoredBrowserDpopKey()).toEqual(proofKey);
    }),
  );

  it.effect("maps IndexedDB open, read, and write failures", () =>
    Effect.gen(function* () {
      installFakeIndexedDb({ fault: "open" });
      expect((yield* Effect.flip(readStoredBrowserDpopKey())).message).toBe(
        "Could not open DPoP key storage.",
      );

      installFakeIndexedDb({ fault: "read", existingStore: true });
      expect((yield* Effect.flip(readStoredBrowserDpopKey())).message).toBe(
        "Could not read DPoP key.",
      );

      const proofKey = yield* generateBrowserDpopKey;
      installFakeIndexedDb({ fault: "write", existingStore: true });
      expect((yield* Effect.flip(writeStoredBrowserDpopKey(proofKey))).message).toBe(
        "Could not write DPoP key.",
      );
    }),
  );

  it.effect("rejects invalid proof URLs", () =>
    Effect.gen(function* () {
      const proofKey = yield* generateBrowserDpopKey;
      const error = yield* createBrowserDpopProof({
        method: "GET",
        url: "not a URL",
        proofKey,
      }).pipe(Effect.provide(browserCryptoLayer), Effect.flip);

      expect(error).toBeInstanceOf(BrowserDpopError);
      expect(error.message).toBe("Could not normalize DPoP proof URL.");
    }),
  );

  it.effect("signs proofs without an access-token hash", () =>
    Effect.gen(function* () {
      const proofKey = yield* generateBrowserDpopKey;
      const { proof } = yield* createBrowserDpopProof({
        method: "get",
        url: "https://relay.example.test/resource#ignored",
        proofKey,
      }).pipe(Effect.provide(browserCryptoLayer));

      expect(decodeJwt(proof)).toMatchObject({
        htm: "GET",
        htu: "https://relay.example.test/resource",
      });
      expect(decodeJwt(proof)).not.toHaveProperty("ath");
    }),
  );

  it.effect("maps key generation and export failures", () =>
    Effect.gen(function* () {
      vi.spyOn(crypto.subtle, "generateKey").mockRejectedValueOnce(new Error("generate failed"));
      expect((yield* Effect.flip(generateBrowserDpopKey)).message).toBe(
        "Could not generate DPoP proof key.",
      );

      vi.spyOn(crypto.subtle, "exportKey").mockRejectedValueOnce(new Error("export failed"));
      expect((yield* Effect.flip(generateBrowserDpopKey)).message).toBe(
        "Could not export DPoP private key.",
      );
    }),
  );

  it.effect("maps public-key export failures", () =>
    Effect.gen(function* () {
      const realExportKey = crypto.subtle.exportKey.bind(crypto.subtle);
      const exportKey = vi.spyOn(crypto.subtle, "exportKey");
      exportKey
        .mockImplementationOnce(realExportKey)
        .mockRejectedValueOnce(new Error("public export failed"));
      expect((yield* Effect.flip(generateBrowserDpopKey)).message).toBe(
        "Could not export DPoP public key.",
      );
    }),
  );

  it.effect("rejects invalid generated public keys", () =>
    Effect.gen(function* () {
      const realExportKey = crypto.subtle.exportKey.bind(crypto.subtle);
      vi.spyOn(crypto.subtle, "exportKey")
        .mockImplementationOnce(realExportKey)
        .mockResolvedValueOnce({ kty: "RSA" });
      expect((yield* Effect.flip(generateBrowserDpopKey)).message).toBe(
        "Generated DPoP public key is invalid.",
      );
    }),
  );
});
