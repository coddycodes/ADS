import type { DesktopProjectDataEnvironmentStatus } from "@coddy/contracts";
import { useState } from "react";

import { Button } from "../ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "../ui/dialog";

interface ProjectDataRecoveryDialogProps {
  readonly open: boolean;
  readonly status: DesktopProjectDataEnvironmentStatus | null;
  readonly busy: boolean;
  readonly error: string | null;
  readonly onOpenChange: (open: boolean) => void;
  readonly onRetry: () => void;
  readonly onRestore: (backupId: string) => Promise<void> | void;
  readonly onStartEmpty: () => Promise<void> | void;
  readonly onOpenPath: () => void;
  readonly onExportDiagnostics: () => void;
  readonly restartError?: string | null;
  readonly requiresStorageAdoption?: boolean;
  readonly onAdoptStorage?: () => void;
}

type Confirmation = "restore" | "start-empty" | null;

export function ProjectDataRecoveryDialog({
  open,
  status,
  busy,
  error,
  onOpenChange,
  onRetry,
  onRestore,
  onStartEmpty,
  onOpenPath,
  onExportDiagnostics,
  restartError = null,
  requiresStorageAdoption = false,
  onAdoptStorage,
}: ProjectDataRecoveryDialogProps) {
  const [selectedBackupId, setSelectedBackupId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation>(null);

  if (!open && (selectedBackupId !== null || confirmation !== null)) {
    setSelectedBackupId(null);
    setConfirmation(null);
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogPopup className="max-w-2xl" showCloseButton={!busy}>
        <DialogHeader>
          <DialogTitle>Project data recovery</DialogTitle>
          <DialogDescription>
            Inspect and recover only the selected local Coddy Studio environment.
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-4">
          {status === null ? (
            <p className="text-sm text-muted-foreground">
              {busy ? "Inspecting project data…" : "This environment could not be inspected."}
            </p>
          ) : (
            <>
              <section className="space-y-1 text-sm">
                <div className="font-medium">{status.label}</div>
                {status.runningDistro ? <div>WSL distribution: {status.runningDistro}</div> : null}
                <div className="break-all">Requested root: {status.requestedRoot}</div>
                <div className="break-all">Effective root: {status.effectiveRoot}</div>
                {status.isFilesystemAlias ? (
                  <div className="text-amber-600">The effective root is a filesystem alias.</div>
                ) : null}
                <div>Storage ID: {status.storageInstanceId ?? "Not available"}</div>
                {status.issue ? <div className="text-destructive">{status.issue}</div> : null}
              </section>

              <section className="space-y-2">
                <h3 className="text-sm font-medium">Verified backups</h3>
                {status.backups.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    No verified backup is available. You can still preserve the current files and
                    start with an empty project database.
                  </p>
                ) : (
                  <div className="space-y-2">
                    {[...status.backups].toReversed().map((backup) => (
                      <label
                        key={backup.backupId}
                        className="flex gap-2 rounded border p-2 text-sm"
                      >
                        <input
                          type="radio"
                          name="project-data-backup"
                          value={backup.backupId}
                          checked={selectedBackupId === backup.backupId}
                          disabled={busy}
                          onChange={() => setSelectedBackupId(backup.backupId)}
                        />
                        <span>
                          {backup.createdAt} · {backup.trigger} · {backup.sizeBytes} bytes
                        </span>
                      </label>
                    ))}
                  </div>
                )}
              </section>

              {confirmation === "restore" ? (
                <div className="rounded border border-amber-500 p-3 text-sm">
                  The selected verified backup will replace the active database. The current files
                  will first be preserved.
                  <div className="mt-2 flex gap-2">
                    <Button disabled={busy} onClick={() => void onRestore(selectedBackupId!)}>
                      Confirm restore
                    </Button>
                    <Button variant="ghost" disabled={busy} onClick={() => setConfirmation(null)}>
                      Cancel
                    </Button>
                  </div>
                </div>
              ) : confirmation === "start-empty" ? (
                <div className="rounded border border-amber-500 p-3 text-sm">
                  The current project database and marker will be preserved, not deleted. Coddy Studio
                  will then start with a new empty database and storage identity.
                  <div className="mt-2 flex gap-2">
                    <Button disabled={busy} onClick={() => void onStartEmpty()}>
                      Confirm start empty
                    </Button>
                    <Button variant="ghost" disabled={busy} onClick={() => setConfirmation(null)}>
                      Cancel
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          )}
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
          {restartError ? (
            <p className="text-sm text-destructive">
              Recovery committed, but the backend could not restart: {restartError}
            </p>
          ) : null}
          {requiresStorageAdoption && restartError === null && onAdoptStorage ? (
            <div className="rounded border p-3 text-sm">
              The new empty database has a new storage identity. Review it before reconnecting.
              <Button className="mt-2" onClick={onAdoptStorage}>
                Use new storage identity
              </Button>
            </div>
          ) : null}
        </DialogPanel>
        <DialogFooter className="flex-wrap">
          <Button variant="outline" disabled={busy} onClick={onRetry}>
            Retry inspection
          </Button>
          <Button variant="outline" disabled={busy || status === null} onClick={onOpenPath}>
            Open data folder
          </Button>
          <Button
            variant="outline"
            disabled={busy || status === null}
            onClick={onExportDiagnostics}
          >
            Export diagnostics
          </Button>
          <Button
            disabled={busy || selectedBackupId === null}
            onClick={() => setConfirmation("restore")}
          >
            Restore selected backup
          </Button>
          <Button
            variant="destructive"
            disabled={busy || status === null}
            onClick={() => setConfirmation("start-empty")}
          >
            Start empty
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
