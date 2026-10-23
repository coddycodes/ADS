import { cn } from "~/lib/utils";
import { type ContextWindowSnapshot, formatContextWindowTokens } from "~/lib/contextWindow";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

function formatPercentage(value: number | null): string | null {
  if (value === null || !Number.isFinite(value)) {
    return null;
  }
  if (value < 10) {
    return `${value.toFixed(1).replace(/\.0$/, "")}%`;
  }
  return `${Math.round(value)}%`;
}

const meterButtonClassName = cn(
  "inline-flex size-6 cursor-pointer items-center justify-center rounded-full border border-transparent text-muted-foreground outline-none transition-colors",
  "hover:bg-accent data-[pressed]:bg-accent",
  "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
);

function ContextWindowRing({
  percentage,
  color = "var(--color-muted-foreground)",
}: {
  percentage: number | null;
  color?: string;
}) {
  const radius = 9.75;
  const circumference = 2 * Math.PI * radius;
  const normalizedPercentage = percentage === null ? null : Math.max(0, Math.min(100, percentage));
  const dashOffset =
    normalizedPercentage === null
      ? undefined
      : circumference - (normalizedPercentage / 100) * circumference;

  return (
    <span className="relative flex size-4 items-center justify-center">
      <svg
        viewBox="0 0 24 24"
        className="-rotate-90 absolute inset-0 size-full transform-gpu"
        aria-hidden="true"
      >
        <circle
          cx="12"
          cy="12"
          r={radius}
          fill="none"
          stroke="color-mix(in oklab, var(--color-muted-foreground) 35%, transparent)"
          strokeWidth="3"
        />
        {normalizedPercentage !== null ? (
          <circle
            cx="12"
            cy="12"
            r={radius}
            fill="none"
            stroke={color}
            strokeWidth="3"
            strokeLinecap="round"
            strokeDasharray={circumference}
            strokeDashoffset={dashOffset}
            className="transition-[stroke-dashoffset] duration-500 ease-out motion-reduce:transition-none"
          />
        ) : null}
      </svg>
    </span>
  );
}

export function ContextWindowMeter(props: {
  supported: boolean;
  usage: ContextWindowSnapshot | null;
  providerDisplayName?: string | null;
}) {
  const { usage, providerDisplayName } = props;
  if (!props.supported) {
    return (
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              className={meterButtonClassName}
              aria-disabled="true"
              aria-label="Context window usage unavailable"
            >
              <ContextWindowRing percentage={null} />
            </button>
          }
        />
        <TooltipPopup side="top">Context usage is not available for this provider.</TooltipPopup>
      </Tooltip>
    );
  }

  if (usage === null) {
    return (
      <Popover>
        <PopoverTrigger
          openOnHover
          delay={150}
          closeDelay={0}
          render={
            <button
              type="button"
              className={meterButtonClassName}
              aria-label="Context window usage awaiting data"
            >
              <ContextWindowRing percentage={null} />
            </button>
          }
        />
        <PopoverPopup tooltipStyle side="top" align="end" className="w-64 max-w-none p-0">
          <div className="flex flex-col gap-2 p-3">
            <div className="font-medium text-muted-foreground text-xs">Context Window</div>
            <div className="text-muted-foreground text-xs">
              Awaiting context usage. Usage will appear after the first provider response.
            </div>
          </div>
        </PopoverPopup>
      </Popover>
    );
  }

  const usedPercentage = formatPercentage(usage.usedPercentage);
  const normalizedPercentage = Math.max(0, Math.min(100, usage.usedPercentage ?? 0));
  const totalProcessedTokens = usage.totalProcessedTokens ?? null;
  const showTotalProcessed = totalProcessedTokens !== null && totalProcessedTokens > 0;
  const isOverloaded = normalizedPercentage > 90;
  const usageColor = isOverloaded ? "var(--color-red-500)" : "var(--color-blue-500)";

  return (
    <Popover>
      <PopoverTrigger
        openOnHover
        delay={150}
        closeDelay={0}
        render={
          <button
            type="button"
            className={meterButtonClassName}
            aria-label={
              usage.maxTokens !== null && usedPercentage
                ? `Context window ${usedPercentage} used`
                : `Context window ${formatContextWindowTokens(usage.usedTokens)} tokens used`
            }
          >
            <ContextWindowRing percentage={normalizedPercentage} color={usageColor} />
          </button>
        }
      />
      <PopoverPopup tooltipStyle side="top" align="end" className="w-64 max-w-none p-0">
        <div className="flex flex-col gap-2 p-3">
          <div className="flex items-center justify-between gap-3">
            <div className="font-medium text-muted-foreground text-xs">Context Window</div>
            {usage.maxTokens !== null && usedPercentage ? (
              <div className="text-xs tabular-nums text-muted-foreground">
                <span>{usedPercentage}</span>
                <span className="mx-1">·</span>
                <span>
                  {formatContextWindowTokens(usage.usedTokens)}/
                  {formatContextWindowTokens(usage.maxTokens ?? null)}
                </span>
              </div>
            ) : (
              <div className="text-xs tabular-nums text-muted-foreground">
                {formatContextWindowTokens(usage.usedTokens)}
              </div>
            )}
          </div>
          {usage.maxTokens !== null ? (
            <div
              className="h-1.5 w-full overflow-hidden rounded-full bg-muted/60"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(normalizedPercentage)}
              aria-label="Context window usage"
            >
              <div
                className="h-full rounded-full transition-[width,background-color] duration-500 ease-out motion-reduce:transition-none"
                style={{ width: `${normalizedPercentage}%`, backgroundColor: usageColor }}
              />
            </div>
          ) : null}
          {showTotalProcessed ? (
            <div className="flex items-center justify-between gap-3 text-xs leading-4">
              <span className="text-muted-foreground">Total processed</span>
              <span className="font-medium tabular-nums text-foreground">
                {formatContextWindowTokens(totalProcessedTokens)}
              </span>
            </div>
          ) : null}
          {usage.compactsAutomatically ? (
            <div className="mt-1 text-pretty text-xs font-medium text-muted-foreground">
              {providerDisplayName ?? "It"} automatically compacts its context
              {usage.autoCompactTokens != null
                ? ` at about ${formatContextWindowTokens(usage.autoCompactTokens)} tokens.`
                : " when needed."}
            </div>
          ) : null}
        </div>
      </PopoverPopup>
    </Popover>
  );
}
