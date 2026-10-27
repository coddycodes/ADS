import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  CONNECTION_DATABASE_NAME,
  deleteIncompatibleConnectionDatabase,
  getConnectionDatabaseHealth,
  monitorConnectionDatabaseOpenRequest,
  resetConnectionDatabaseHealthForTest,
  subscribeConnectionDatabaseHealth,
} from "./databaseHealth";

class FakeRequest extends EventTarget implements IDBOpenDBRequest {
  error: DOMException | null = null;
  onblocked: ((this: IDBOpenDBRequest, ev: IDBVersionChangeEvent) => unknown) | null = null;
  onerror: ((this: IDBRequest<IDBDatabase>, ev: Event) => unknown) | null = null;
  onsuccess: ((this: IDBRequest<IDBDatabase>, ev: Event) => unknown) | null = null;
  onupgradeneeded: ((this: IDBOpenDBRequest, ev: IDBVersionChangeEvent) => unknown) | null = null;
  readyState: IDBRequestReadyState = "pending";
  result = {} as IDBDatabase;
  source = {} as IDBObjectStore;
  transaction = null;

  fire(type: string): void {
    if (this.readyState === "done") throw new Error("IndexedDB request already settled");
    if (type === "success" || type === "error") this.readyState = "done";
    this.dispatchEvent(new Event(type));
  }
}

afterEach(() => {
  resetConnectionDatabaseHealthForTest();
  vi.unstubAllGlobals();
});

describe("connection database health", () => {
  it("classifies version conflicts, blocked opens, generic failures, and later success", () => {
    const snapshots: string[] = [];
    const unsubscribe = subscribeConnectionDatabaseHealth(() => {
      snapshots.push(getConnectionDatabaseHealth().status);
    });

    const blocked = new FakeRequest();
    monitorConnectionDatabaseOpenRequest(blocked);
    blocked.fire("blocked");
    expect(getConnectionDatabaseHealth().status).toBe("blocked");

    const ready = new FakeRequest();
    monitorConnectionDatabaseOpenRequest(ready);
    ready.fire("success");
    expect(getConnectionDatabaseHealth().status).toBe("ready");

    const incompatible = new FakeRequest();
    incompatible.error = new DOMException("newer database", "VersionError");
    monitorConnectionDatabaseOpenRequest(incompatible);
    incompatible.fire("error");
    expect(getConnectionDatabaseHealth()).toEqual({
      status: "incompatible",
      message: "This browser cannot open connection data written by a newer Coddy Studio version.",
    });

    const unavailable = new FakeRequest();
    unavailable.error = new DOMException("permission denied", "UnknownError");
    monitorConnectionDatabaseOpenRequest(unavailable);
    unavailable.fire("error");
    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "unavailable" });
    expect(snapshots).toEqual(["blocked", "ready", "incompatible", "unavailable"]);
    unsubscribe();
  });

  it("classifies WebKit's permanent database-file failure as incompatible", () => {
    const incompatible = new FakeRequest();
    incompatible.error = new DOMException("Unable to establish IDB database file", "UnknownError");
    monitorConnectionDatabaseOpenRequest(incompatible);
    incompatible.fire("error");

    expect(getConnectionDatabaseHealth()).toEqual({
      status: "incompatible",
      message:
        "This Coddy Studio build uses an older browser engine than the one that last wrote its connection data, so the connection database cannot be opened.",
    });
  });

  it("keeps a blocked deletion pending until that request succeeds", async () => {
    const open = new FakeRequest();
    open.error = new DOMException("newer database", "VersionError");
    monitorConnectionDatabaseOpenRequest(open);
    open.fire("error");
    const request = new FakeRequest();
    const deleteDatabase = vi.fn(() => request);
    vi.stubGlobal("indexedDB", { deleteDatabase });
    let outcome: "pending" | "deleted" | "blocked" | "rejected" = "pending";

    const deletion = deleteIncompatibleConnectionDatabase().then(
      (result) => {
        outcome = result;
        return result;
      },
      (cause: unknown) => {
        outcome = "rejected";
        throw cause;
      },
    );
    request.fire("blocked");
    await Promise.resolve();

    expect(outcome).toBe("pending");
    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "blocked" });
    expect(deleteDatabase).toHaveBeenCalledWith(CONNECTION_DATABASE_NAME);

    request.fire("success");
    await expect(deletion).resolves.toBe("deleted");
    expect(getConnectionDatabaseHealth().status).toBe("ready");
  });

  it("rejects a blocked deletion that later errors and publishes unavailable health", async () => {
    const request = new FakeRequest();
    vi.stubGlobal("indexedDB", {
      deleteDatabase: () => request,
    });
    const deletion = deleteIncompatibleConnectionDatabase();

    request.fire("blocked");
    request.error = new DOMException("deletion denied", "UnknownError");
    request.fire("error");

    await expect(deletion).rejects.toThrow("deletion denied");
    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "unavailable" });
  });

  it("keeps queued deletion health authoritative over competing open events", async () => {
    const open = new FakeRequest();
    monitorConnectionDatabaseOpenRequest(open);
    const deletion = new FakeRequest();
    vi.stubGlobal("indexedDB", { deleteDatabase: () => deletion });

    const pending = deleteIncompatibleConnectionDatabase();
    deletion.fire("blocked");
    open.fire("success");

    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "blocked" });
    deletion.fire("success");
    await expect(pending).resolves.toBe("deleted");
    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "ready" });
  });

  it("publishes open faults observed after a deletion begins", async () => {
    // A deletion must never permanently mute fault reporting: only open
    // requests that predate it are suppressed.
    const deletion = new FakeRequest();
    vi.stubGlobal("indexedDB", { deleteDatabase: () => deletion });
    const pending = deleteIncompatibleConnectionDatabase();
    deletion.fire("blocked");

    const laterOpen = new FakeRequest();
    laterOpen.error = new DOMException("permission denied", "UnknownError");
    monitorConnectionDatabaseOpenRequest(laterOpen);
    laterOpen.fire("error");
    expect(getConnectionDatabaseHealth()).toMatchObject({ status: "unavailable" });

    deletion.fire("success");
    await expect(pending).resolves.toBe("deleted");
  });

  it("settles a permanently blocked deletion with a descriptive failure", async () => {
    vi.useFakeTimers();
    try {
      const deletion = new FakeRequest();
      vi.stubGlobal("indexedDB", { deleteDatabase: () => deletion });
      const pending = deleteIncompatibleConnectionDatabase();
      deletion.fire("blocked");

      // Attach the rejection expectation before the timer fires so the
      // settle rejection is never momentarily unhandled.
      const settleFailure = expect(pending).rejects.toThrow("Close them and try again");
      await vi.advanceTimersByTimeAsync(15_000);
      await settleFailure;
      expect(getConnectionDatabaseHealth()).toMatchObject({ status: "blocked" });

      // Health reporting keeps flowing afterwards.
      const laterOpen = new FakeRequest();
      monitorConnectionDatabaseOpenRequest(laterOpen);
      laterOpen.fire("success");
      expect(getConnectionDatabaseHealth().status).toBe("ready");
    } finally {
      vi.useRealTimers();
    }
  });

  it("publishes ready only after successful deletion", async () => {
    const open = new FakeRequest();
    open.error = new DOMException("newer database", "VersionError");
    monitorConnectionDatabaseOpenRequest(open);
    open.fire("error");
    const deletion = new FakeRequest();
    vi.stubGlobal("indexedDB", {
      deleteDatabase: () => {
        queueMicrotask(() => deletion.fire("success"));
        return deletion;
      },
    });

    await expect(deleteIncompatibleConnectionDatabase()).resolves.toBe("deleted");
    expect(getConnectionDatabaseHealth().status).toBe("ready");
  });
});
