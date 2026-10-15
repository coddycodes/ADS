import type {
  DeviceKey,
  DevicePlatform,
  DevicePlatformCatalog,
  DeviceSnapshot,
} from "@bibcode/contracts";
import {
  ChevronLeftIcon,
  CircleIcon,
  ClipboardPasteIcon,
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
import {
  Select,
  SelectGroup,
  SelectGroupLabel,
  SelectItem,
  SelectPopup,
  SelectTrigger,
  SelectValue,
} from "../ui/select";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  DEVICE_DISABLED_REASONS,
  deviceStateLabel,
  IN_USE_ELSEWHERE_SUFFIX,
  isViewOnlyAvailability,
  PASTE_HINTS,
  PLATFORM_LABELS,
  STILL_IMAGES_LABEL,
  type StillImages,
} from "./deviceMessages";

export type RotateDirection = "left" | "right";

/** Which image the picker shows; ids are unique only within a platform. */
export interface DeviceImageSelection {
  readonly platform: DevicePlatform;
  readonly imageId: string;
}

/** The picker's string value for an image. A platform name never contains `:`. */
const pickerValue = (platform: DevicePlatform, id: string) => `${platform}:${id}`;

/** A platform's availability message in the picker; not a platform, so never an image's value. */
const noticeValue = (platform: DevicePlatform) => `notice:${platform}`;

/** A control that is either usable or says why not, in its tooltip and to screen readers. */
function ToolbarButton({
  label,
  hint,
  disabledReason,
  onClick,
  children,
  iconOnly = true,
}: {
  readonly label: string;
  /** The tooltip while enabled, when it says more than `label`. */
  readonly hint?: string;
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
      title={disabledReason === null ? (hint ?? label) : undefined}
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

/** The picker's entries for one platform: its images, then why it cannot run (or runs view only). */
function PlatformGroup({ listed }: { readonly listed: DevicePlatformCatalog }) {
  const { platform, availability, images } = listed;
  const opens = availability.kind === "ready" || isViewOnlyAvailability(availability.kind);
  return (
    <SelectGroup>
      <SelectGroupLabel>{PLATFORM_LABELS[platform]}</SelectGroupLabel>
      {opens
        ? images.map((image) => {
            const value = pickerValue(platform, image.id);
            return (
              <SelectItem
                hideIndicator
                key={value}
                value={value}
                disabled={image.inUseElsewhere === true}
              >
                {image.inUseElsewhere === true
                  ? `${image.displayName}${IN_USE_ELSEWHERE_SUFFIX}`
                  : image.displayName}
              </SelectItem>
            );
          })
        : null}
      {availability.kind === "ready" ? null : (
        <SelectItem
          hideIndicator
          disabled
          value={noticeValue(platform)}
          className="whitespace-normal text-xs text-muted-foreground"
        >
          {availability.message}
        </SelectItem>
      )}
    </SelectGroup>
  );
}

export function DeviceToolbar({
  platforms,
  selectedImage,
  selectedName,
  device,
  startDisabledReason,
  pickerLocked,
  viewOnly,
  onSelectImage,
  onStart,
  onStop,
  onRotate,
  onKey,
  onPaste,
  stillImages = null,
}: {
  /** The catalog's platforms, each a group in the picker. */
  readonly platforms: ReadonlyArray<DevicePlatformCatalog>;
  readonly selectedImage: DeviceImageSelection | null;
  /** The selected image's display name. */
  readonly selectedName: string | null;
  /** The device this pane shows; `null` before one is started. */
  readonly device: DeviceSnapshot | null;
  /** Why Start is disabled, or `null` when it can start the selected image. */
  readonly startDisabledReason: string | null;
  readonly pickerLocked: boolean;
  /** The shown device's platform runs view only: no keys, paste or rotation. */
  readonly viewOnly: boolean;
  readonly onSelectImage: (platform: DevicePlatform, imageId: string) => void;
  readonly onStart: () => void;
  readonly onStop: () => void;
  readonly onRotate: (direction: RotateDirection) => void;
  readonly onKey: (key: DeviceKey) => void;
  readonly onPaste: () => void;
  /** Set while the screen shows still images instead of video, and why. */
  readonly stillImages?: StillImages | null;
}) {
  const state = device?.state ?? null;
  const running = state === "starting" || state === "booting" || state === "ready";
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
  const inputReason = readyReason ?? (viewOnly ? DEVICE_DISABLED_REASONS.viewOnly : null);
  const rotateReason =
    inputReason ?? (device?.canRotate === false ? DEVICE_DISABLED_REASONS.cannotRotate : null);
  const platform = device?.platform ?? selectedImage?.platform ?? "android";
  const ios = platform === "ios";
  const selectedValue =
    selectedImage === null ? "" : pickerValue(selectedImage.platform, selectedImage.imageId);
  const hasImages = platforms.some((listed) => listed.images.length > 0);

  return (
    <div
      className="flex min-w-0 flex-wrap items-center gap-1.5 border-b border-border/60 px-2 py-1.5"
      role="toolbar"
      aria-label="Device controls"
    >
      <Select
        value={selectedValue}
        disabled={pickerLocked || !hasImages}
        onValueChange={(value) => {
          for (const listed of platforms) {
            const picked = listed.images.find(
              (image) => pickerValue(listed.platform, image.id) === value,
            );
            if (picked !== undefined) return onSelectImage(listed.platform, picked.id);
          }
        }}
      >
        <SelectTrigger aria-label="Virtual device" size="sm" className="w-40 min-w-0">
          <SelectValue>{selectedName ?? "No device"}</SelectValue>
        </SelectTrigger>
        <SelectPopup align="start" alignItemWithTrigger={false}>
          {platforms.map((listed) => (
            <PlatformGroup key={listed.platform} listed={listed} />
          ))}
        </SelectPopup>
      </Select>
      <ToolbarButton
        label="Start"
        disabledReason={startDisabledReason}
        onClick={onStart}
        iconOnly={false}
      >
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
        disabledReason={rotateReason}
        onClick={() => onRotate("left")}
      >
        <RotateCcwIcon />
      </ToolbarButton>
      <ToolbarButton
        label="Rotate right"
        disabledReason={rotateReason}
        onClick={() => onRotate("right")}
      >
        <RotateCwIcon />
      </ToolbarButton>
      {ios ? null : (
        <ToolbarButton label="Back" disabledReason={inputReason} onClick={() => onKey("back")}>
          <ChevronLeftIcon />
        </ToolbarButton>
      )}
      <ToolbarButton label="Home" disabledReason={inputReason} onClick={() => onKey("home")}>
        <CircleIcon />
      </ToolbarButton>
      {ios ? null : (
        <ToolbarButton
          label="Recents"
          disabledReason={inputReason}
          onClick={() => onKey("appSwitch")}
        >
          <SquareStackIcon />
        </ToolbarButton>
      )}
      <ToolbarButton
        label={ios ? "Lock" : "Power"}
        disabledReason={inputReason}
        onClick={() => onKey("power")}
      >
        <PowerIcon />
      </ToolbarButton>
      <ToolbarButton
        label="Volume down"
        disabledReason={inputReason}
        onClick={() => onKey("volumeDown")}
      >
        <Volume1Icon />
      </ToolbarButton>
      <ToolbarButton
        label="Volume up"
        disabledReason={inputReason}
        onClick={() => onKey("volumeUp")}
      >
        <Volume2Icon />
      </ToolbarButton>
      <ToolbarButton
        label="Paste"
        hint={PASTE_HINTS[platform]}
        disabledReason={inputReason}
        onClick={onPaste}
      >
        <ClipboardPasteIcon />
      </ToolbarButton>
      <div className="ml-auto flex min-w-0 items-center gap-1.5">
        {stillImages === null ? null : (
          <Tooltip>
            <TooltipTrigger
              render={
                <Badge
                  variant="outline"
                  size="sm"
                  tabIndex={0}
                  data-still-images={stillImages.reason}
                >
                  {STILL_IMAGES_LABEL}
                </Badge>
              }
            />
            <TooltipPopup>{stillImages.message}</TooltipPopup>
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
