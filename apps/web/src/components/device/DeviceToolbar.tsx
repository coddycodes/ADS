import type { DeviceKey, DevicePlatform, DeviceSnapshot } from "@bibcode/contracts";
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
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  DEVICE_DISABLED_REASONS,
  deviceStateLabel,
  STILL_IMAGES_LABEL,
  STILL_IMAGES_REASONS,
  type StillImagesReason,
} from "./deviceMessages";

export type RotateDirection = "left" | "right";

/** An image the picker offers, with the platform that runs it. */
export interface DevicePickerImage {
  readonly platform: DevicePlatform;
  readonly id: string;
  readonly displayName: string;
  readonly inUseElsewhere?: true;
}

/** Which image the picker shows; ids are unique only within a platform. */
export interface DeviceImageSelection {
  readonly platform: DevicePlatform;
  readonly imageId: string;
}

/** The picker's string value for an image. A platform name never contains `:`. */
const pickerValue = (platform: DevicePlatform, id: string) => `${platform}:${id}`;

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
  images,
  selectedImage,
  device,
  startPending,
  onSelectImage,
  onStart,
  onStop,
  onRotate,
  onKey,
  stillImagesReason = null,
}: {
  readonly images: ReadonlyArray<DevicePickerImage>;
  readonly selectedImage: DeviceImageSelection | null;
  /** The device this pane shows; `null` before one is started. */
  readonly device: DeviceSnapshot | null;
  readonly startPending: boolean;
  readonly onSelectImage: (platform: DevicePlatform, imageId: string) => void;
  readonly onStart: () => void;
  readonly onStop: () => void;
  readonly onRotate: (direction: RotateDirection) => void;
  readonly onKey: (key: DeviceKey) => void;
  /** Set while the screen shows still images instead of video, and why. */
  readonly stillImagesReason?: StillImagesReason | null;
}) {
  const state = device?.state ?? null;
  const running = state === "starting" || state === "booting" || state === "ready";
  const startReason =
    images.length === 0
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
  const selectedValue =
    selectedImage === null ? "" : pickerValue(selectedImage.platform, selectedImage.imageId);
  const selectedLabel =
    images.find((image) => pickerValue(image.platform, image.id) === selectedValue)?.displayName ??
    selectedImage?.imageId ??
    "No device";
  const pickerLocked = running || state === "stopping" || startPending;

  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-1.5 border-b border-border/60 px-2 py-1.5"
      role="toolbar"
      aria-label="Device controls"
    >
      <Select
        value={selectedValue}
        disabled={pickerLocked || images.length === 0}
        onValueChange={(value) => {
          const picked = images.find((image) => pickerValue(image.platform, image.id) === value);
          if (picked !== undefined) onSelectImage(picked.platform, picked.id);
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
          {images.map((image) => {
            const value = pickerValue(image.platform, image.id);
            return (
              <SelectItem hideIndicator key={value} value={value}>
                {image.displayName}
              </SelectItem>
            );
          })}
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
      <div className="ml-auto flex min-w-0 items-center gap-1.5">
        {stillImagesReason === null ? null : (
          <Tooltip>
            <TooltipTrigger
              render={
                <Badge
                  variant="outline"
                  size="sm"
                  tabIndex={0}
                  data-still-images={stillImagesReason}
                >
                  {STILL_IMAGES_LABEL}
                </Badge>
              }
            />
            <TooltipPopup>{STILL_IMAGES_REASONS[stillImagesReason]}</TooltipPopup>
          </Tooltip>
        )}
        {device === null ? null : (
          <span
            role="status"
            className={cn(
              "max-w-full truncate rounded-full px-2 py-0.5 text-xs font-medium",
              PILL_TONES[device.state],
            )}
            title={deviceStateLabel(device)}
          >
            {deviceStateLabel(device)}
          </span>
        )}
      </div>
    </div>
  );
}
