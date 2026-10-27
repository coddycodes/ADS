import type { UploadProgress } from "@coddy/client-runtime/operations";

import { formatTransferBytes } from "../../lib/formatTransferBytes";
import { Button } from "../ui/button";

export interface PendingAttachmentUpload extends UploadProgress {
  readonly attachmentCount: number;
  readonly fileName: string;
}

export function attachmentUploadCopy(progress: PendingAttachmentUpload): string {
  if (progress.phase === "reconnecting") return "Reconnecting…";
  const attachments = progress.attachmentCount === 1 ? "attachment" : "attachments";
  return `Uploading ${progress.attachmentCount} ${attachments} — ${formatTransferBytes(progress.sentBytes, progress.totalBytes)}`;
}

export function attachmentUploadFailureCopy(fileName: string, cause: string | null): string {
  return cause === null
    ? `Couldn't upload "${fileName}". Your message is back in the composer. Try sending it again.`
    : `Couldn't upload "${fileName}": ${cause}. Your message is back in the composer.`;
}

export function AttachmentUploadNotice({
  progress,
  onCancel,
}: {
  readonly progress: PendingAttachmentUpload;
  readonly onCancel: () => void;
}) {
  return (
    <div className="flex w-full max-w-[80%] items-center gap-2 text-xs text-muted-foreground">
      <p role="status" className="min-w-0 flex-1 wrap-break-word">
        {attachmentUploadCopy(progress)}
      </p>
      <Button type="button" size="xs" variant="ghost" onClick={onCancel}>
        Cancel
      </Button>
    </div>
  );
}
