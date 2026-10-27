import { type TurnDelivery } from "@coddy/contracts";

import { deliveryOffersRetry } from "../ChatView.logic";
import { Button } from "../ui/button";

export interface TurnDeliveryNoticeProps {
  readonly delivery: TurnDelivery;
  readonly providerLabel: string;
  readonly waitingBehind?: { readonly offersRetry: boolean } | null;
  readonly onRetry: () => void;
  readonly onDismiss: () => void;
  readonly disabled: boolean;
}

export function TurnDeliveryNotice({
  delivery,
  providerLabel,
  waitingBehind,
  onRetry,
  onDismiss,
  disabled,
}: TurnDeliveryNoticeProps) {
  if (delivery.state === "delivered" && delivery.reason === "startedNewConversation") {
    return (
      <p role="status" className="w-full max-w-[80%] wrap-break-word text-xs text-muted-foreground">
        Sent in a new conversation with a summary of earlier messages.
      </p>
    );
  }
  if (delivery.state === "pending" && waitingBehind) {
    return (
      <p role="status" className="w-full max-w-[80%] wrap-break-word text-xs text-muted-foreground">
        {waitingBehind.offersRetry
          ? "Waiting for an earlier message. Retry or dismiss it to send this one."
          : "Waiting for an earlier message. Dismiss it to send this one."}
      </p>
    );
  }
  if (delivery.state !== "uncertain" && delivery.state !== "failed") {
    return null;
  }

  const uncertain = delivery.state === "uncertain";
  const offersRetry = deliveryOffersRetry(delivery);
  // A failed or uncertain delivery holds back the thread's later deliveries until the user
  // retries or dismisses it, and Retry resends it unchanged (same model and options). The copy
  // says both instead of promising that Retry clears the problem.
  const guidance = uncertain
    ? `${providerLabel} may have received this message, and later messages wait behind it. Retrying could deliver a duplicate; Dismiss skips it.`
    : offersRetry
      ? `${providerLabel} did not receive this message, and later messages wait behind it. Retry sends it again unchanged; Dismiss skips it.`
      : "Sending it again unchanged would fail, and later messages wait behind it. Dismiss it, then send it again with another model or without that option.";

  return (
    <div
      role="status"
      className={`flex w-full max-w-[80%] flex-col gap-1.5 border-s-2 px-2.5 py-1.5 text-xs sm:flex-row sm:items-center sm:gap-3 ${
        uncertain
          ? "border-warning/50 bg-warning/5 text-warning-foreground"
          : "border-destructive/50 bg-destructive/5 text-destructive-foreground"
      }`}
    >
      <div className="min-w-0 flex-1">
        <p className="font-medium">{uncertain ? "Delivery uncertain" : "Delivery failed"}</p>
        {delivery.detail ? (
          <p className="wrap-break-word text-muted-foreground">{delivery.detail}</p>
        ) : null}
        <p className="text-muted-foreground">{guidance}</p>
      </div>
      <div className="flex shrink-0 items-center gap-1 self-end sm:self-auto">
        {offersRetry ? (
          <Button
            type="button"
            size="xs"
            variant="ghost"
            disabled={disabled}
            onClick={onRetry}
            aria-label="Retry message delivery"
          >
            Retry
          </Button>
        ) : null}
        <Button
          type="button"
          size="xs"
          variant="ghost"
          disabled={disabled}
          onClick={onDismiss}
          aria-label="Dismiss and skip this message"
        >
          Dismiss
        </Button>
      </div>
    </div>
  );
}
