import type { EnvironmentId } from "@coddy/contracts";

import { RemoteDirectoryBrowser } from "../RemoteDirectoryBrowser";
import { Dialog, DialogDescription, DialogHeader, DialogPopup, DialogTitle } from "../ui/dialog";

export interface RemoteDirectoryPickerDialogProps {
  readonly open: boolean;
  readonly environmentId: EnvironmentId;
  readonly initialPath: string;
  readonly onOpenChange: (open: boolean) => void;
  readonly onSelect: (path: string) => void;
}

export function RemoteDirectoryPickerDialog({
  open,
  environmentId,
  initialPath,
  onOpenChange,
  onSelect,
}: RemoteDirectoryPickerDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-xl">
        <DialogHeader className="pb-4">
          <DialogTitle>Select Workspace folder</DialogTitle>
          <DialogDescription>Browse directories on the selected Coddy Studio host.</DialogDescription>
        </DialogHeader>
        <div className="px-6 pb-5">
          <RemoteDirectoryBrowser
            environmentId={environmentId}
            initialPath={initialPath}
            // Base UI keeps the popup (and this browser) mounted through its ~200ms exit
            // transition after `open` flips to false, so the key must stay stable across
            // that transition; it only needs to change when the browsing target does.
            resetKey={environmentId}
            onSelect={onSelect}
            onCancel={() => onOpenChange(false)}
          />
        </div>
      </DialogPopup>
    </Dialog>
  );
}
