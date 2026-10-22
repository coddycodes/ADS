import { useCopyToClipboard } from "~/hooks/useCopyToClipboard";
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
import { stackedThreadToast, toastManager } from "../ui/toast";

export interface ManualUpdateStepsRequest {
  readonly name: string;
  readonly steps: string;
}

export function ManualUpdateStepsDialog({
  request,
  onClose,
}: {
  readonly request: ManualUpdateStepsRequest | null;
  readonly onClose: () => void;
}) {
  const { copyToClipboard, isCopied } = useCopyToClipboard<void>({
    target: "manual update instructions",
    onCopy: () => {
      toastManager.add({ type: "success", title: "Update instructions copied" });
    },
    onError: (error) => {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not copy update instructions",
          description: error.message,
        }),
      );
    },
  });
  if (request === null) return null;
  return (
    <Dialog
      open
      // Read-only steps: nothing is lost, so an outside press may close it.
      disablePointerDismissal={false}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogPopup className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Update {request.name} manually</DialogTitle>
          <DialogDescription>Run these steps on the server's host.</DialogDescription>
        </DialogHeader>
        <DialogPanel>
          <div data-text-surface="background" className="bg-background">
            <div className="overflow-x-auto rounded-md">
              <pre data-text-surface="card" className="whitespace-pre-wrap bg-card p-3 text-xs">
                {request.steps}
              </pre>
            </div>
          </div>
        </DialogPanel>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>
            Close
          </Button>
          <Button type="button" onClick={() => copyToClipboard(request.steps, undefined)}>
            {isCopied ? "Copied" : "Copy"}
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
