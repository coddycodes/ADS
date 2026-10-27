export const CONNECTION_DATABASE_NAME = "coddy-studio:connection-runtime";
export const CONNECTION_DATABASE_VERSION = 2;

export type ConnectionDatabaseHealthStatus = "ready" | "incompatible" | "blocked" | "unavailable";

export interface ConnectionDatabaseHealth {
  readonly status: ConnectionDatabaseHealthStatus;
  readonly message: string | null;
}

const READY_HEALTH: ConnectionDatabaseHealth = { status: "ready", message: null };
const DELETION_BLOCKED_SETTLE_TIMEOUT_MS = 15_000;
let health: ConnectionDatabaseHealth = READY_HEALTH;
/**
 * Starting a deletion bumps the generation, so open requests that predate it
 * cannot clobber the deletion's status — while every later open publishes
 * normally. A deletion can therefore never permanently mute fault reporting.
 */
let openGeneration = 0;
const listeners = new Set<() => void>();

function publish(next: ConnectionDatabaseHealth): void {
  health = next;
  for (const listener of listeners) listener();
}

function errorName(error: unknown): string | null {
  if (typeof error !== "object" || error === null || !("name" in error)) return null;
  return typeof error.name === "string" ? error.name : null;
}

export function monitorConnectionDatabaseOpenRequest(request: IDBOpenDBRequest): void {
  const generation = openGeneration;
  const publishOpenHealth = (next: ConnectionDatabaseHealth) => {
    if (generation === openGeneration) publish(next);
  };
  request.addEventListener("blocked", () => {
    publishOpenHealth({
      status: "blocked",
      message: "Another tab or process is holding an older connection database open.",
    });
  });
  request.addEventListener("error", () => {
    const error = request.error;
    if (errorName(error) === "VersionError") {
      publishOpenHealth({
        status: "incompatible",
        message: "This browser cannot open connection data written by a newer Coddy Studio version.",
      });
      return;
    }
    // This WebKit message is the only IndexedDB API signal for this permanent
    // on-disk condition, and is verbatim in WebKitGTK 2.50 and 2.52.
    if (
      errorName(error) === "UnknownError" &&
      error?.message.includes("Unable to establish IDB database file")
    ) {
      publishOpenHealth({
        status: "incompatible",
        message:
          "This Coddy Studio build uses an older browser engine than the one that last wrote its connection data, so the connection database cannot be opened.",
      });
      return;
    }
    publishOpenHealth({
      status: "unavailable",
      message: `The connection database could not be opened: ${String(error ?? "unknown error")}`,
    });
  });
  request.addEventListener("success", () => publishOpenHealth(READY_HEALTH));
}

export function reportConnectionDatabaseUnavailable(cause: unknown): void {
  publish({
    status: "unavailable",
    message: `The connection database is unavailable: ${String(cause)}`,
  });
}

export function getConnectionDatabaseHealth(): ConnectionDatabaseHealth {
  return health;
}

export function subscribeConnectionDatabaseHealth(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function deleteIncompatibleConnectionDatabase(): Promise<"deleted"> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB is unavailable in this browser context."));
      return;
    }
    let settled = false;
    let blockedTimer: ReturnType<typeof setTimeout> | undefined;
    const clearBlockedTimer = () => {
      if (blockedTimer !== undefined) {
        clearTimeout(blockedTimer);
        blockedTimer = undefined;
      }
    };
    // Invalidate pre-deletion open monitors; later opens publish normally.
    openGeneration += 1;
    const request = indexedDB.deleteDatabase(CONNECTION_DATABASE_NAME);
    request.addEventListener("blocked", () => {
      publish({
        status: "blocked",
        message:
          "The browser queued connection database deletion until other Coddy Studio tabs and windows close.",
      });
      // A deletion the browser keeps queued must not leave the caller
      // waiting forever; settle with a descriptive failure while the queued
      // deletion itself remains pending in the browser.
      if (blockedTimer !== undefined) return;
      blockedTimer = setTimeout(() => {
        if (settled) return;
        settled = true;
        reject(
          new Error(
            "Deletion stayed blocked by other Coddy Studio tabs or windows. Close them and try again.",
          ),
        );
      }, DELETION_BLOCKED_SETTLE_TIMEOUT_MS);
    });
    request.addEventListener("error", () => {
      clearBlockedTimer();
      const error = request.error ?? new Error("Unknown IndexedDB deletion error.");
      publish({
        status: "unavailable",
        message: `The connection database could not be deleted: ${String(error)}`,
      });
      if (settled) return;
      settled = true;
      reject(error);
    });
    request.addEventListener("success", () => {
      clearBlockedTimer();
      publish(READY_HEALTH);
      if (settled) return;
      settled = true;
      resolve("deleted");
    });
  });
}

/** @internal Test isolation for the renderer-wide external store. */
export function resetConnectionDatabaseHealthForTest(): void {
  openGeneration += 1;
  publish(READY_HEALTH);
}
