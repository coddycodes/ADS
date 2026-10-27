import { memo, type ReactNode } from "react";
import { ArrowUp, Clock, X } from "lucide-react";
import type { MessageId } from "@coddy/contracts";
import type { TimestampFormat } from "@coddy/contracts/settings";
import type { ChatMessage } from "../../types";
import type { QueuedCardStatus } from "../ChatView.logic";
import { formatChatTimestampTooltip, formatShortTimestamp } from "../../timestampFormat";
import { Button } from "../ui/button";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

interface QueuedMessageTimelineRowProps {
  message: ChatMessage;
  status: QueuedCardStatus;
  isHead: boolean;
  children: ReactNode;
  timestampFormat: TimestampFormat;
  resolving: boolean;
  error: string | null;
  onSteer: (messageId: MessageId) => void;
  onSendNow: (messageId: MessageId) => void;
  onCancel: (messageId: MessageId) => void;
}

export const QueuedMessageTimelineRow = memo(function QueuedMessageTimelineRow({
  message,
  status,
  isHead,
  children,
  timestampFormat,
  resolving,
  error,
  onSteer,
  onSendNow,
  onCancel,
}: QueuedMessageTimelineRowProps) {
  const sendNow = status.primaryAction === "send-now";
  const primaryLabel = sendNow ? "Send now" : "Steer into the running turn";
  const primaryReason = resolving
    ? "Updating this queued message…"
    : sendNow
      ? status.sendNowDisabledReason
      : status.steerDisabledReason;
  const primaryDisabled = resolving || (sendNow ? !status.canSendNow : !status.canSteer);
  const cancelDisabled = resolving || !status.canCancel;
  const cancelReason = resolving
    ? "Updating this queued message…"
    : status.steering
      ? "Steering…"
      : "Cancel and return to the composer";
  const attachmentCount = message.attachments?.length ?? 0;

  return (
    <div className="group flex flex-col items-end gap-1" data-queued-message-row={message.id}>
      <div className="max-w-[80%] rounded-2xl border border-dashed border-border bg-secondary/60 p-3">
        {children}
        {attachmentCount > 0 ? (
          <p className="mt-2 text-xs text-muted-foreground">
            {attachmentCount} {attachmentCount === 1 ? "attachment" : "attachments"}
          </p>
        ) : null}
        {message.delivery?.detail ? (
          <p className="mt-2 text-xs text-muted-foreground">{message.delivery.detail}</p>
        ) : null}
        {error ? (
          <p role="alert" className="mt-2 text-xs text-destructive-foreground">
            {error}
          </p>
        ) : null}
        <div
          role="status"
          className="mt-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground"
        >
          <Clock className="size-3.5 shrink-0" aria-hidden="true" />
          <span>Queued</span>
          <span className="min-w-0 flex-1">{status.label}</span>
          {status.primaryAction ? (
            <Tooltip>
              <TooltipTrigger render={<span tabIndex={primaryDisabled ? 0 : undefined} />}>
                <Button
                  type="button"
                  size="xs"
                  variant="outline"
                  aria-label={primaryLabel}
                  aria-description={primaryReason ?? undefined}
                  disabled={primaryDisabled}
                  data-queued-message-steer={sendNow ? undefined : "true"}
                  data-queued-message-send-now={sendNow ? "true" : undefined}
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => (sendNow ? onSendNow(message.id) : onSteer(message.id))}
                >
                  <ArrowUp className="size-3.5" aria-hidden="true" />
                  {sendNow ? "Send now" : "Steer"}
                </Button>
              </TooltipTrigger>
              <TooltipPopup>{primaryReason ?? primaryLabel}</TooltipPopup>
            </Tooltip>
          ) : null}
          <Tooltip>
            <TooltipTrigger render={<span tabIndex={cancelDisabled ? 0 : undefined} />}>
              <Button
                type="button"
                size="xs"
                variant="ghost"
                aria-label="Cancel and return to the composer"
                aria-description={cancelDisabled ? cancelReason : undefined}
                disabled={cancelDisabled}
                data-queued-message-cancel="true"
                onPointerDown={(event) => event.preventDefault()}
                onClick={() => onCancel(message.id)}
              >
                <X className="size-4" aria-hidden="true" />
              </Button>
            </TooltipTrigger>
            <TooltipPopup>{cancelReason}</TooltipPopup>
          </Tooltip>
        </div>
        {isHead && status.primaryAction === null && status.steerDisabledReason ? (
          <p className="mt-1 text-xs text-muted-foreground">{status.steerDisabledReason}</p>
        ) : null}
      </div>
      <div className="pe-1 text-xs tabular-nums text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
        <Tooltip>
          <TooltipTrigger render={<span />}>
            {formatShortTimestamp(message.createdAt, timestampFormat)}
          </TooltipTrigger>
          <TooltipPopup>
            {formatChatTimestampTooltip(message.createdAt, timestampFormat)}
          </TooltipPopup>
        </Tooltip>
      </div>
    </div>
  );
});
