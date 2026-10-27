import type { ConsumeCodexRateLimitResetResult } from "@coddy/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@coddy/client-runtime/state/runtime";
import * as DateTime from "effect/DateTime";
import { useRef, useState } from "react";

import { formatRelativeTimeLabel } from "../../timestampFormat";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../ui/alert-dialog";
import { Button } from "../ui/button";
import { ProviderUsageWindowMeter } from "./ProviderUsageWindowMeter";
import type { ProviderUsageViewModel } from "./providerUsagePresentation";

export type ConsumeCodexRateLimitReset = (
  requestId: string,
) => Promise<AtomCommandResult<ConsumeCodexRateLimitResetResult, unknown>>;

export interface ProviderUsageDetailProps {
  readonly viewModel: ProviderUsageViewModel;
  readonly onOpenProviderSettings: () => void;
  readonly onConsumeCodexRateLimitReset?: ConsumeCodexRateLimitReset | undefined;
  readonly isResetting?: boolean | undefined;
}

interface ResetNotice {
  readonly message: string;
  readonly tone: "neutral" | "success" | "warning" | "destructive";
}

const RESET_OUTCOME_NOTICE: Record<ConsumeCodexRateLimitResetResult["outcome"], ResetNotice> = {
  reset: { message: "Rate-limit reset completed.", tone: "success" },
  nothingToReset: {
    message: "No eligible windows currently need a reset.",
    tone: "neutral",
  },
  noCredit: { message: "No reset credit is available.", tone: "warning" },
  alreadyRedeemed: {
    message: "This reset request was already redeemed.",
    tone: "warning",
  },
};

const NOTICE_TEXT_CLASS: Record<ResetNotice["tone"], string> = {
  neutral: "text-muted-foreground",
  success: "text-success",
  warning: "text-warning",
  destructive: "text-destructive",
};

function providerLabel(provider: ProviderUsageViewModel["provider"]): string {
  return provider === "claude" ? "Claude" : "Codex";
}

function resetFailureMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim().length > 0) {
    return `${error.message} Try again.`;
  }
  return "Could not reset Codex rate limits. Try again.";
}

function providerStateCopy(viewModel: ProviderUsageViewModel): string | null {
  if (viewModel.error !== null) {
    return viewModel.windows.length > 0
      ? `Showing last available usage. ${viewModel.error}`
      : viewModel.error;
  }
  if (viewModel.windows.length > 0) return null;
  switch (viewModel.status) {
    case "fetching":
      return "Loading usage…";
    case "unavailable":
      return "Usage is unavailable for this provider.";
    default:
      return "No usage windows are available yet.";
  }
}

function ResetNotice({ notice }: { readonly notice: ResetNotice }) {
  return (
    <p
      aria-atomic="true"
      aria-live="polite"
      className={`text-xs ${NOTICE_TEXT_CLASS[notice.tone]}`}
      role="status"
    >
      {notice.message}
    </p>
  );
}

export function ProviderUsageDetail({
  viewModel,
  onOpenProviderSettings,
  onConsumeCodexRateLimitReset,
  isResetting = false,
}: ProviderUsageDetailProps) {
  const [isConfirmOpen, setIsConfirmOpen] = useState(false);
  const [isAttemptPending, setIsAttemptPending] = useState(false);
  const [resetNotice, setResetNotice] = useState<ResetNotice | null>(null);
  const activeRequestIdRef = useRef<string | null>(null);

  const updatedAt = DateTime.formatIso(viewModel.updatedAt);
  const stateCopy = providerStateCopy(viewModel);
  const pending = isAttemptPending || isResetting;
  const canReset =
    viewModel.provider === "codex" &&
    viewModel.credits !== null &&
    viewModel.credits.availableCount > 0 &&
    onConsumeCodexRateLimitReset !== undefined;

  const confirmReset = async () => {
    if (
      pending ||
      activeRequestIdRef.current !== null ||
      onConsumeCodexRateLimitReset === undefined
    ) {
      return;
    }

    // @effect-diagnostics-next-line cryptoRandomUUID:off -- The reset protocol requires one browser UUID per confirmed attempt.
    const requestId = crypto.randomUUID();
    activeRequestIdRef.current = requestId;
    setIsConfirmOpen(false);
    setIsAttemptPending(true);
    setResetNotice({ message: "Resetting eligible windows…", tone: "neutral" });

    try {
      const result = await onConsumeCodexRateLimitReset(requestId);
      if (result._tag === "Success") {
        setResetNotice(RESET_OUTCOME_NOTICE[result.value.outcome]);
      } else if (isAtomCommandInterrupted(result)) {
        setResetNotice({ message: "Codex reset was interrupted. Try again.", tone: "warning" });
      } else {
        setResetNotice({
          message: resetFailureMessage(squashAtomCommandFailure(result)),
          tone: "destructive",
        });
      }
    } catch (error) {
      setResetNotice({ message: resetFailureMessage(error), tone: "destructive" });
    } finally {
      activeRequestIdRef.current = null;
      setIsAttemptPending(false);
    }
  };

  return (
    <div
      className="w-[min(22.5rem,calc(100vw-1rem))] max-w-full space-y-3 p-3 text-xs"
      data-testid="provider-usage-detail"
    >
      <div className="flex min-w-0 items-start justify-between gap-3 border-border border-b pb-2">
        <div className="min-w-0">
          <h2 className="font-medium text-foreground text-sm">
            {providerLabel(viewModel.provider)} usage
          </h2>
          {viewModel.provider === "codex" && viewModel.plan !== null ? (
            <p className="mt-0.5 text-muted-foreground">{viewModel.plan.label} plan</p>
          ) : null}
        </div>
        <p className="shrink-0 text-muted-foreground text-xs">
          Updated{" "}
          <time dateTime={updatedAt} title={updatedAt}>
            {formatRelativeTimeLabel(updatedAt)}
          </time>
        </p>
      </div>

      {viewModel.detailedWindows.length > 0 ? (
        <div className="space-y-3">
          {viewModel.detailedWindows.map((window) => (
            <ProviderUsageWindowMeter key={window.key} variant="detail" window={window} />
          ))}
        </div>
      ) : null}

      {stateCopy !== null ? (
        <p
          aria-atomic="true"
          aria-live="polite"
          className={viewModel.error === null ? "text-muted-foreground" : "text-warning"}
          role="status"
        >
          {stateCopy}
        </p>
      ) : null}

      {viewModel.provider === "codex" && viewModel.credits !== null ? (
        <div className="flex min-w-0 items-center justify-between gap-3 border-border border-t pt-3">
          <div className="min-w-0">
            <p className="font-medium text-foreground">
              {viewModel.credits.availableCount} reset credit
              {viewModel.credits.availableCount === 1 ? "" : "s"}
            </p>
            {viewModel.credits.nextExpiresLabel !== null ? (
              <p className="mt-0.5 truncate text-muted-foreground">
                Next expiry: {viewModel.credits.nextExpiresLabel}
              </p>
            ) : null}
          </div>
          {canReset ? (
            <Button
              aria-busy={pending}
              aria-label={pending ? "Resetting Codex rate limits" : "Reset Codex rate limits now"}
              disabled={pending}
              size="xs"
              variant="outline"
              onClick={() => setIsConfirmOpen(true)}
            >
              {pending ? "Resetting…" : "Reset now"}
            </Button>
          ) : null}
        </div>
      ) : null}

      {viewModel.provider === "codex" && resetNotice !== null ? (
        <ResetNotice notice={resetNotice} />
      ) : null}

      <div className="border-border border-t pt-2">
        <Button className="px-0" size="xs" variant="link" onClick={onOpenProviderSettings}>
          Provider settings
        </Button>
      </div>

      <AlertDialog
        open={isConfirmOpen}
        onOpenChange={(open) => {
          if (!pending) setIsConfirmOpen(open);
        }}
      >
        <AlertDialogPopup>
          <AlertDialogHeader>
            <AlertDialogTitle>Reset Codex rate limits?</AlertDialogTitle>
            <AlertDialogDescription>
              One reset credit will be consumed, and eligible windows will reset immediately.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogClose render={<Button variant="outline" />}>Cancel</AlertDialogClose>
            <Button disabled={pending} onClick={() => void confirmReset()}>
              Confirm reset
            </Button>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </div>
  );
}
