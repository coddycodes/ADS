import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import {
  deleteIncompatibleConnectionDatabase,
  getConnectionDatabaseHealth,
  subscribeConnectionDatabaseHealth,
} from "../connection/databaseHealth";
import { connectionCatalogLivesInIndexedDb } from "../connection/storage";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";

const CATALOG_DELETED_DATA = [
  "Saved remote servers and connection profiles",
  "Connection credentials and remote DPoP tokens",
  "Accepted storage identities",
] as const;
const CACHE_DELETED_DATA = ["Cached environment shell state", "Cached thread state"] as const;

/**
 * The reset drops only the IndexedDB database. On protected desktop hosts the
 * connection catalog lives in the native store, so promising its destruction
 * here would be false purge assurance.
 */
function deletedDataForActiveBackend(): ReadonlyArray<string> {
  return connectionCatalogLivesInIndexedDb()
    ? [...CATALOG_DELETED_DATA, ...CACHE_DELETED_DATA]
    : [...CACHE_DELETED_DATA];
}

interface ConnectionDatabaseRecoveryDialogProps {
  readonly deleteDatabase?: typeof deleteIncompatibleConnectionDatabase;
  readonly reloadPage?: () => void;
  readonly copyText?: (text: string) => Promise<void>;
}

const reloadCurrentPage = () => window.location.reload();
const writeClipboardText = (text: string) => navigator.clipboard.writeText(text);

export function ConnectionDatabaseRecoveryDialog({
  deleteDatabase = deleteIncompatibleConnectionDatabase,
  reloadPage = reloadCurrentPage,
  copyText = writeClipboardText,
}: ConnectionDatabaseRecoveryDialogProps = {}) {
  const health = useSyncExternalStore(
    subscribeConnectionDatabaseHealth,
    getConnectionDatabaseHealth,
    getConnectionDatabaseHealth,
  );
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetAcknowledged, setResetAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const acknowledgementRef = useRef<HTMLInputElement>(null);
  const open = health.status !== "ready";

  useEffect(() => {
    if (confirmReset) acknowledgementRef.current?.focus();
  }, [confirmReset]);

  const reset = async () => {
    setBusy(true);
    setError(null);
    try {
      await deleteDatabase();
      reloadPage();
    } catch (cause) {
      setConfirmReset(false);
      setResetAcknowledged(false);
      const message = `The connection database could not be deleted: ${String(cause)}`;
      if (getConnectionDatabaseHealth().message !== message) setError(message);
    } finally {
      setBusy(false);
    }
  };
  const copyDiagnostics = async () => {
    setError(null);
    try {
      await copyText(
        `Coddy Studio connection database status: ${health.status}\n${health.message ?? "No detail"}`,
      );
    } catch (cause) {
      setError(`Could not copy diagnostics: ${String(cause)}`);
    }
  };

  return (
    <Dialog open={open} onOpenChange={() => undefined}>
      <DialogPopup showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>
            {health.status === "incompatible"
              ? "Connection database needs reset"
              : health.status === "blocked"
                ? "Connection database is blocked"
                : "Connection storage is unavailable"}
          </DialogTitle>
          <DialogDescription role={health.status === "unavailable" ? "alert" : undefined}>
            {health.message}
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-4 text-sm">
          {health.status === "incompatible" ? (
            <>
              <p>Resetting deletes only this browser&apos;s connection-runtime data:</p>
              <ul className="list-disc space-y-1 pl-5">
                {deletedDataForActiveBackend().map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
              <p className="text-muted-foreground">
                Server-side project files and databases are not changed.
              </p>
              {confirmReset ? (
                <div
                  id="connection-database-reset-confirmation"
                  role="status"
                  className="space-y-3 rounded border border-amber-500 p-3"
                >
                  <p>Deleting all saved browser connection data cannot be undone.</p>
                  <label className="flex items-center gap-2">
                    <input
                      ref={acknowledgementRef}
                      type="checkbox"
                      checked={resetAcknowledged}
                      disabled={busy}
                      className="accent-primary size-4"
                      onChange={(event) => setResetAcknowledged(event.target.checked)}
                    />
                    I understand that this deletes the connection data listed above.
                  </label>
                  <Button
                    variant="destructive"
                    disabled={!resetAcknowledged || busy}
                    onClick={() => void reset()}
                  >
                    {busy ? "Deleting…" : "Delete saved connection data"}
                  </Button>
                </div>
              ) : null}
            </>
          ) : health.status === "blocked" ? (
            <div className="space-y-2">
              <p>Close other Coddy Studio tabs or windows using this browser profile, then reload.</p>
              {busy ? (
                <p role="status">
                  Deletion is still pending and will continue when those connections close.
                </p>
              ) : null}
            </div>
          ) : (
            <p>Reload to retry. If the problem continues, copy diagnostics before reporting it.</p>
          )}
          {error ? (
            <p role="alert" className="text-destructive">
              {error}
            </p>
          ) : null}
        </DialogPanel>
        <DialogFooter>
          {health.status === "unavailable" ? (
            <Button variant="outline" onClick={() => void copyDiagnostics()}>
              Copy diagnostics
            </Button>
          ) : null}
          {health.status === "incompatible" ? (
            <Button
              variant="destructive"
              disabled={busy}
              aria-expanded={confirmReset}
              aria-controls={confirmReset ? "connection-database-reset-confirmation" : undefined}
              onClick={() => {
                setConfirmReset(true);
                setResetAcknowledged(false);
              }}
            >
              Reset saved connection data
            </Button>
          ) : null}
          <Button onClick={reloadPage}>Reload</Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
