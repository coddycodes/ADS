import type { DeviceKey, DeviceSnapshot } from "@bibcode/contracts";
import {
  ChevronLeftIcon,
  CircleIcon,
  PlayIcon,
  PowerIcon,
  RotateCcwIcon,
  RotateCwIcon,
  SquareIcon,
  SquareStackIcon,
  Volume1Icon,
  Volume2Icon,
} from "lucide-react";
import { type ReactNode, useId } from "react";

import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { DEVICE_DISABLED_REASONS, deviceStateLabel } from "./deviceMessages";

export type RotateDirection = "left" | "right";

/** A control that is either usable or says why not, in its tooltip and to screen readers. */
function ToolbarButton({
  label,
  disabledReason,
  onClick,
  children,
  iconOnly = true,
}: {
  readonly label: string;
  readonly disabledReason: string | null;
  readonly onClick: () => void;
  readonly children: ReactNode;
  readonly iconOnly?: boolean;
}) {
  const reasonId = useId();
  const button = (
    <Button
      size={iconOnly ? "icon-xs" : "xs"}
      variant={iconOnly ? "ghost" : "outline"}
      aria-label={iconOnly ? label : undefined}
      title={disabledReason === null ? label : undefined}
      disabled={disabledReason !== null}
      aria-describedby={disabledReason === null ? undefined : reasonId}
      onClick={onClick}
    >
      {children}
    </Button>
  );
  if (disabledReason === null) return button;
  // A disabled button gets no pointer events, so the wrapper carries the reason.
  return (
    <span className="inline-flex" title={disabledReason} data-disabled-reason={disabledReason}>
      {button}
      <span id={reasonId} className="sr-only">
        {disabledReason}
      </span>
    </span>
  );
}

const PILL_TONES: Readonly<Record<DeviceSnapshot["state"], string>> = {
  starting: "bg-muted text-foreground",
  booting: "bg-muted text-foreground",
  ready: "bg-success/12 text-success",
  stopping: "bg-muted text-foreground",
  stopped: "bg-muted text-muted-foreground",
  failed: "bg-destructive/12 text-destructive",
};

export function DeviceToolbar({
  avds,
  selectedAvd,
  device,
  startPending,
  onSelectAvd,
  onStart,
  onStop,
  onRotate,
  onKey,
}: {
  readonly avds: ReadonlyArray<{ readonly name: string; readonly displayName: string }>;
  readonly selectedAvd: string | null;
  /** The device this pane shows; `null` before one is started. */
  readonly device: DeviceSnapshot | null;
  readonly startPending: boolean;
  readonly onSelectAvd: (avdName: string) => void;
  readonly onStart: () => void;
  readonly onStop: () => void;
  readonly onRotate: (direction: RotateDirection) => void;
  readonly onKey: (key: DeviceKey) => void;
}) {
  const state = device?.state ?? null;
  const running = state === "starting" || state === "booting" || state === "ready";
  const startReason =
    avds.length === 0
      ? DEVICE_DISABLED_REASONS.noAvd
      : startPending
        ? DEVICE_DISABLED_REASONS.starting
        : running
          ? DEVICE_DISABLED_REASONS.running
          : state === "stopping"
            ? DEVICE_DISABLED_REASONS.stopping
            : null;
  const stopReason = running
    ? null
    : state === "stopping"
      ? DEVICE_DISABLED_REASONS.stopping
      : DEVICE_DISABLED_REASONS.startFirst;
  const readyReason =
    state === "ready"
      ? null
      : state === "starting" || state === "booting"
        ? DEVICE_DISABLED_REASONS.notBooted
        : state === "stopping"
          ? DEVICE_DISABLED_REASONS.stopping
          : DEVICE_DISABLED_REASONS.startFirst;
  const selectedLabel =
    avds.find((avd) => avd.name === selectedAvd)?.displayName ?? selectedAvd ?? "No device";
  const pickerLocked = running || state === "stopping" || startPending;

  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-1.5 border-b border-border/60 px-2 py-1.5"
      role="toolbar"
      aria-label="Device controls"
    >
      <Select
        value={selectedAvd ?? ""}
        disabled={pickerLocked || avds.length === 0}
        onValueChange={(value) => {
          if (typeof value === "string" && value.length > 0) onSelectAvd(value);
        }}
      >
        <SelectTrigger
          aria-label="Virtual device"
          size="sm"
          className="w-40 min-w-0"
          title={startReason ?? undefined}
        >
          <SelectValue>{selectedLabel}</SelectValue>
        </SelectTrigger>
        <SelectPopup align="start" alignItemWithTrigger={false}>
          {avds.map((avd) => (
            <SelectItem hideIndicator key={avd.name} value={avd.name}>
              {avd.displayName}
            </SelectItem>
          ))}
        </SelectPopup>
      </Select>
      <ToolbarButton label="Start" disabledReason={startReason} onClick={onStart} iconOnly={false}>
        <PlayIcon />
        Start
      </ToolbarButton>
      <ToolbarButton label="Stop" disabledReason={stopReason} onClick={onStop} iconOnly={false}>
        <SquareIcon />
        Stop
      </ToolbarButton>
      <span className="mx-0.5 h-4 w-px bg-border" aria-hidden />
      <ToolbarButton
        label="Rotate left"
        disabledReason={readyReason}
        onClick={() => onRotate("left")}
      >
        <RotateCcwIcon />
      </ToolbarButton>
      <ToolbarButton
        label="Rotate right"
        disabledReason={readyReason}
        onClick={() => onRotate("right")}
      >
        <RotateCwIcon />
      </ToolbarButton>
      <ToolbarButton label="Back" disabledReason={readyReason} onClick={() => onKey("back")}>
        <ChevronLeftIcon />
      </ToolbarButton>
      <ToolbarButton label="Home" disabledReason={readyReason} onClick={() => onKey("home")}>
        <CircleIcon />
      </ToolbarButton>
      <ToolbarButton
        label="Recents"
        disabledReason={readyReason}
        onClick={() => onKey("appSwitch")}
      >
        <SquareStackIcon />
      </ToolbarButton>
      <ToolbarButton label="Power" disabledReason={readyReason} onClick={() => onKey("power")}>
        <PowerIcon />
      </ToolbarButton>
      <ToolbarButton
        label="Volume down"
        disabledReason={readyReason}
        onClick={() => onKey("volumeDown")}
      >
        <Volume1Icon />
      </ToolbarButton>
      <ToolbarButton
        label="Volume up"
        disabledReason={readyReason}
        onClick={() => onKey("volumeUp")}
      >
        <Volume2Icon />
      </ToolbarButton>
      {device === null ? null : (
        <span
          role="status"
          className={cn(
            "ml-auto max-w-full truncate rounded-full px-2 py-0.5 text-xs font-medium",
            PILL_TONES[device.state],
          )}
          title={deviceStateLabel(device)}
        >
          {deviceStateLabel(device)}
        </span>
      )}
    </div>
  );
}
