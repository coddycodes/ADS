import type { StatusBarUsageMode } from "@coddy/contracts/settings";
import { useState } from "react";

import { ClaudeAI, OpenAI } from "../Icons";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { ProviderUsageDetail, type ProviderUsageDetailProps } from "./ProviderUsageDetail";
import { ProviderUsageWindowMeter } from "./ProviderUsageWindowMeter";
import type { ProviderUsageViewModel } from "./providerUsagePresentation";

function providerLabel(provider: ProviderUsageViewModel["provider"]): string {
  return provider === "claude" ? "Claude" : "Codex";
}

function ProviderIcon({ provider }: { readonly provider: ProviderUsageViewModel["provider"] }) {
  return provider === "claude" ? (
    <ClaudeAI aria-hidden className="size-3 shrink-0" data-provider-icon="claude" />
  ) : (
    <OpenAI aria-hidden className="size-3 shrink-0 text-foreground" data-provider-icon="codex" />
  );
}

function ProviderUsageBadge({ viewModel }: { readonly viewModel: ProviderUsageViewModel }) {
  const hasData = viewModel.windows.length > 0;
  return (
    <span
      aria-hidden
      className="inline-flex items-center gap-1 text-muted-foreground"
      data-provider-usage-badge={viewModel.provider}
    >
      <ProviderIcon provider={viewModel.provider} />
      <span
        className={`size-1.5 rounded-full ${
          hasData ? "bg-muted-foreground/60" : "bg-muted-foreground/30"
        }`}
      />
    </span>
  );
}

function ProviderUsageSummary({
  mode,
  viewModel,
}: {
  readonly mode: StatusBarUsageMode;
  readonly viewModel: ProviderUsageViewModel;
}) {
  const windows = mode === "detailed" ? viewModel.detailedWindows : viewModel.compactWindows;
  return (
    <span className="inline-flex min-w-0 items-center gap-1.5" data-provider={viewModel.provider}>
      <ProviderIcon provider={viewModel.provider} />
      {windows.length === 0 ? (
        <span className="font-mono text-muted-foreground">--</span>
      ) : (
        windows.map((window) => (
          <ProviderUsageWindowMeter key={window.key} variant="footer" window={window} />
        ))
      )}
    </span>
  );
}

export interface ProviderUsageControlProps {
  readonly viewModel: ProviderUsageViewModel;
  readonly statusBarUsageMode: StatusBarUsageMode;
  readonly iconOnly: boolean;
  readonly onOpenProviderSettings: () => void;
  readonly onConsumeCodexRateLimitReset?: ProviderUsageDetailProps["onConsumeCodexRateLimitReset"];
}

export function ProviderUsageControl({
  viewModel,
  statusBarUsageMode,
  iconOnly,
  onOpenProviderSettings,
  onConsumeCodexRateLimitReset,
}: ProviderUsageControlProps) {
  const [open, setOpen] = useState(false);
  const label = `${providerLabel(viewModel.provider)} usage`;
  const handleOpenProviderSettings = () => {
    setOpen(false);
    onOpenProviderSettings();
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        aria-label={label}
        className="inline-flex h-5 shrink-0 items-center rounded border-0 bg-transparent px-1 text-xs outline-none hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-ring"
        title={label}
      >
        {iconOnly ? (
          <ProviderUsageBadge viewModel={viewModel} />
        ) : (
          <ProviderUsageSummary mode={statusBarUsageMode} viewModel={viewModel} />
        )}
      </PopoverTrigger>
      <PopoverPopup
        align="start"
        aria-label={`${label} details`}
        className="w-[min(22.5rem,calc(100vw-1rem))] max-w-[calc(100vw-1rem)]"
        side="top"
        viewportClassName="p-0 [--viewport-inline-padding:0px]"
      >
        <ProviderUsageDetail
          viewModel={viewModel}
          onConsumeCodexRateLimitReset={onConsumeCodexRateLimitReset}
          onOpenProviderSettings={handleOpenProviderSettings}
        />
      </PopoverPopup>
    </Popover>
  );
}
