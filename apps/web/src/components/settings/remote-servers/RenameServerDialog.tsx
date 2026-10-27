import { SERVER_NAME_REQUIRED_MESSAGE } from "@coddy/client-runtime/connection";
import type { EnvironmentId } from "@coddy/contracts";
import { useEffect, useId, useRef, useState } from "react";

import { Button } from "../../ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "../../ui/dialog";
import { Input } from "../../ui/input";
import { describeRenameServerFailure } from "./connectPresentation";

export interface RenameServerRequest {
  readonly environmentId: EnvironmentId;
  /** The name this device currently shows for the server. */
  readonly label: string;
  /** The name the server reports for itself, when it is connected. */
  readonly serverLabel: string | null;
}

export interface RenameServerDialogProps {
  readonly request: RenameServerRequest | null;
  readonly onClose: () => void;
  /** Saves the name; resolves with a message to show, or null once it is saved. */
  readonly onRename: (environmentId: EnvironmentId, label: string) => Promise<string | null>;
}

/** Renames a saved server on this device. The server's own name is unchanged. */
export function RenameServerDialog({ request, onClose, onRename }: RenameServerDialogProps) {
  return request === null ? null : (
    <RenameServerDialogBody
      key={request.environmentId}
      request={request}
      onClose={onClose}
      onRename={onRename}
    />
  );
}

function RenameServerDialogBody({
  request,
  onClose,
  onRename,
}: {
  readonly request: RenameServerRequest;
  readonly onClose: () => void;
  readonly onRename: RenameServerDialogProps["onRename"];
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const formId = useId();
  const hintId = useId();
  const [name, setName] = useState(request.label);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trimmedName = name.trim();
  const nameMissing = trimmedName.length === 0;
  const serverLabel = request.serverLabel?.trim() || null;
  const offerServerName = serverLabel !== null && serverLabel !== trimmedName;

  // Select the current name so typing replaces it.
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      input.select();
    });
    return () => cancelAnimationFrame(frame);
  }, []);

  const save = async () => {
    if (nameMissing || saving) return;
    if (trimmedName === request.label) {
      onClose();
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const failure = await onRename(request.environmentId, trimmedName);
      if (failure === null) {
        onClose();
      } else {
        setError(failure);
      }
    } catch (cause) {
      setError(describeRenameServerFailure(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !saving) onClose();
      }}
    >
      <DialogPopup className="max-w-md" showCloseButton={!saving}>
        <DialogHeader>
          <DialogTitle>Rename server</DialogTitle>
          <DialogDescription>The new name shows on this device only.</DialogDescription>
        </DialogHeader>
        <DialogPanel>
          <form
            id={formId}
            className="grid gap-1.5"
            aria-busy={saving}
            onSubmit={(event) => {
              event.preventDefault();
              void save();
            }}
          >
            <label className="grid gap-1.5">
              <span className="text-xs font-medium text-foreground">Name</span>
              <Input
                ref={inputRef}
                aria-label="Server name"
                value={name}
                disabled={saving}
                aria-invalid={nameMissing}
                aria-describedby={nameMissing ? hintId : undefined}
                spellCheck={false}
                onChange={(event) => {
                  setName(event.target.value);
                  setError(null);
                }}
              />
            </label>
            {nameMissing ? (
              <p id={hintId} className="text-xs text-muted-foreground">
                {SERVER_NAME_REQUIRED_MESSAGE}
              </p>
            ) : null}
            {offerServerName ? (
              <Button
                type="button"
                variant="link"
                size="xs"
                className="h-auto justify-self-start px-0 text-muted-foreground underline underline-offset-2 hover:text-foreground"
                disabled={saving}
                onClick={() => {
                  setName(serverLabel);
                  setError(null);
                  inputRef.current?.focus();
                }}
              >
                Use the server’s name: {serverLabel}
              </Button>
            ) : null}
            {error === null ? null : (
              <p aria-live="polite" className="text-xs text-destructive">
                {error}
              </p>
            )}
          </form>
        </DialogPanel>
        <DialogFooter>
          <Button type="button" variant="outline" disabled={saving} onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form={formId} disabled={nameMissing || saving}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
