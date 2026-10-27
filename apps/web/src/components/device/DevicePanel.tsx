import type {
  DeviceCatalog,
  DeviceImage,
  DeviceInputEvent,
  DeviceKey,
  DeviceOrientation,
  DevicePlatform,
  DevicePlatformCatalog,
  DeviceSnapshot,
  EnvironmentId,
} from "@coddy/contracts";
import {
  type AtomCommandResult,
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";
import { useNavigate } from "@tanstack/react-router";
import * as Cause from "effect/Cause";
import { SmartphoneIcon, TriangleAlertIcon } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";

import { deviceEnvironment } from "../../state/device";
import { useEnvironmentQuery } from "../../state/query";
import { useAtomCommand } from "../../state/use-atom-command";
import { Button } from "../ui/button";
import { RetryButton } from "../ui/retry-button";
import { useDevicePreviewsAvailability } from "./deviceAvailability";
import { DeviceInputQueue } from "./deviceInputQueue";
import { isUsTypeable, splitDeviceText } from "./deviceKeyMap";
import {
  availabilityTitle,
  CLIPBOARD_UNREADABLE_MESSAGE,
  DEVICE_DISABLED_REASONS,
  DEVICE_PREVIEWS_OFF_MESSAGE,
  DEVICE_PREVIEWS_SETTINGS_ANCHOR,
  DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE,
  type DeviceErrorView,
  describeDeviceError,
  deviceStateLabel,
  IOS_PASTE_NOTE,
  isViewOnlyAvailability,
  rotateKeptNote,
  type StillImages,
} from "./deviceMessages";
import { DeviceScreen } from "./DeviceScreen";
import { type DeviceImageSelection, DeviceToolbar, type RotateDirection } from "./DeviceToolbar";

/** Turning the device left goes portrait → landscape → reverse portrait → reverse landscape. */
const ORIENTATIONS_LEFT: ReadonlyArray<DeviceOrientation> = [
  "portrait",
  "landscape",
  "reversePortrait",
  "reverseLandscape",
];

export function rotatedOrientation(
  current: DeviceOrientation,
  direction: RotateDirection,
): DeviceOrientation {
  const index = ORIENTATIONS_LEFT.indexOf(current);
  const step = direction === "left" ? 1 : ORIENTATIONS_LEFT.length - 1;
  return ORIENTATIONS_LEFT[(index + step) % ORIENTATIONS_LEFT.length]!;
}

/** How long the note that a rotate did nothing stays up. */
const ROTATE_NOTE_MS = 5_000;
/** How long a refused input's message stays up after the latest refusal. */
const INPUT_ERROR_MS = 5_000;

const isRunning = (device: DeviceSnapshot) =>
  device.state !== "stopped" && device.state !== "failed";

/** Whether a platform's images open: it is ready, or runs them view only. */
const opensImages = (listed: DevicePlatformCatalog) =>
  listed.availability.kind === "ready" || isViewOnlyAvailability(listed.availability.kind);

const selectionOf = (device: DeviceSnapshot): DeviceImageSelection => ({
  platform: device.platform,
  imageId: device.imageId,
});

const isSelected = (device: DeviceSnapshot, selection: DeviceImageSelection) =>
  device.platform === selection.platform && device.imageId === selection.imageId;

function findImage(
  catalog: DeviceCatalog,
  platform: DevicePlatform,
  imageId: string,
): DeviceImage | undefined {
  return catalog.platforms
    .find((listed) => listed.platform === platform)
    ?.images.find((image) => image.id === imageId);
}

/** An image's display name, else its id (an AVD name, or a simulator's UDID). */
export function imageDisplayName(
  catalog: DeviceCatalog | null,
  platform: DevicePlatform,
  imageId: string,
): string {
  return (
    (catalog === null ? undefined : findImage(catalog, platform, imageId))?.displayName ?? imageId
  );
}

/**
 * The image the picker shows: the user's pick while the catalog lists it, else the device this
 * pane started while it runs, else any running device, else the one this pane started (so its
 * failure stays visible), else the first image that can start. The catalog sorts devices by
 * id, not age, so the device this pane started is the only sign of which is newest.
 */
export function selectedImageOf(
  catalog: DeviceCatalog | null,
  picked: DeviceImageSelection | null,
  followedDeviceId: string | null,
): DeviceImageSelection | null {
  if (catalog === null) return null;
  if (picked !== null && findImage(catalog, picked.platform, picked.imageId) !== undefined) {
    return picked;
  }
  const followed = catalog.devices.find((device) => device.deviceId === followedDeviceId);
  if (followed !== undefined && isRunning(followed)) return selectionOf(followed);
  const running = catalog.devices.find(isRunning);
  if (running !== undefined) return selectionOf(running);
  if (followed !== undefined && followed.state === "failed") return selectionOf(followed);
  for (const listed of catalog.platforms) {
    if (!opensImages(listed)) continue;
    const free = listed.images.find((image) => image.inUseElsewhere !== true);
    if (free !== undefined) return { platform: listed.platform, imageId: free.id };
  }
  return null;
}

/**
 * The device the pane shows: the selected image's running device (one per platform at most),
 * else the one this pane started on it, if that failed, so its reason stays visible.
 */
export function selectShownDevice(
  catalog: DeviceCatalog | null,
  selected: DeviceImageSelection | null,
  followedDeviceId: string | null,
): DeviceSnapshot | null {
  if (catalog === null || selected === null) return null;
  return (
    catalog.devices.find((device) => isRunning(device) && isSelected(device, selected)) ??
    catalog.devices.find(
      (device) =>
        device.deviceId === followedDeviceId &&
        device.state === "failed" &&
        isSelected(device, selected),
    ) ??
    null
  );
}

function PanelMessage({
  title,
  children,
  action,
}: {
  readonly title?: string;
  readonly children: ReactNode;
  readonly action?: ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-6">
      <div className="flex max-w-sm flex-col items-center gap-2 text-center">
        <SmartphoneIcon aria-hidden className="size-5 text-muted-foreground" />
        {title === undefined ? null : (
          <h3 className="text-sm font-medium text-foreground">{title}</h3>
        )}
        <p className="text-xs text-muted-foreground">{children}</p>
        {action}
      </div>
    </div>
  );
}

function OpenSettingsButton({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const navigate = useNavigate();
  return (
    <Button
      size="xs"
      variant="outline"
      onClick={() => {
        void navigate({
          to: "/settings/mobile",
          hash: `${DEVICE_PREVIEWS_SETTINGS_ANCHOR}-${environmentId}`,
        });
      }}
    >
      Open settings
    </Button>
  );
}

/** The right-panel device pane for one environment's running device. */
export function DevicePanel({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const availability = useDevicePreviewsAvailability(environmentId);
  if (!availability.known) {
    return <PanelMessage>Connecting to the server…</PanelMessage>;
  }
  if (!availability.supported) {
    return <PanelMessage>{DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE}</PanelMessage>;
  }
  if (!availability.enabled) {
    return (
      <PanelMessage action={<OpenSettingsButton environmentId={environmentId} />}>
        {DEVICE_PREVIEWS_OFF_MESSAGE}
      </PanelMessage>
    );
  }
  // Keyed so switching to a thread on another environment starts from a clean pane.
  return <DeviceSession key={environmentId} environmentId={environmentId} />;
}

function isDisabledFailure(cause: Cause.Cause<unknown>): boolean {
  const error = Cause.squash(cause);
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { _tag?: unknown })._tag === "DevicePreviewsDisabledError"
  );
}

interface InputLane {
  readonly deviceId: string;
  readonly queue: DeviceInputQueue;
  alive: boolean;
}

/** A failed device action, with the platform a failed start was for. */
interface PaneError extends DeviceErrorView {
  readonly platform: DevicePlatform | null;
}

/** Whether the pane has had the iOS paste note out for a device, and whether it still shows. */
interface PasteNote {
  readonly deviceId: string;
  readonly open: boolean;
}

/** A rotate that left this device as it was; each refusal is a new note with its own timer. */
interface RotateNote {
  readonly deviceId: string;
}

/**
 * What a settled rotate left a device in, while the catalog may not say so yet; it holds only
 * as long as the catalog still shows `catalogOrientation`, what it showed when Rotate was pressed.
 */
interface LastTurn {
  readonly deviceId: string;
  readonly orientation: DeviceOrientation;
  readonly catalogOrientation: DeviceOrientation;
}

/** Why the shown device refused input, or why it could not be sent. */
interface InputErrorNote {
  readonly deviceId: string;
  readonly message: string;
}

/** Each platform's reason it cannot run devices, under its own heading. */
function UnavailablePlatforms({
  platforms,
}: {
  readonly platforms: ReadonlyArray<DevicePlatformCatalog>;
}) {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-6">
      <div className="flex max-w-sm flex-col items-center gap-4 text-center">
        <SmartphoneIcon aria-hidden className="size-5 text-muted-foreground" />
        {platforms.length === 0 ? (
          <p className="text-xs text-muted-foreground">This server can't run device previews.</p>
        ) : null}
        {platforms.map((listed) => (
          <div key={listed.platform} className="flex flex-col gap-1">
            <h3 className="text-sm font-medium text-foreground">
              {availabilityTitle(listed.availability.kind)}
            </h3>
            <p className="text-xs text-muted-foreground">{listed.availability.message}</p>
          </div>
        ))}
      </div>
    </div>
  );
}

function DeviceSession({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const catalogQuery = useEnvironmentQuery(deviceEnvironment.catalog({ environmentId, input: {} }));
  const { data: catalog, emission, revalidate } = catalogQuery;
  // The server ends the catalog with "off" while previews are off. A cached stream that ended
  // that way before they were turned on is read again once, rather than stuck on "off".
  const disabledFailure = emission._tag === "Failure" && isDisabledFailure(emission.cause);
  const revalidatedRef = useRef(false);
  useEffect(() => {
    if (!disabledFailure || revalidatedRef.current) return;
    revalidatedRef.current = true;
    revalidate();
  }, [disabledFailure, revalidate]);

  const runOpen = useAtomCommand(deviceEnvironment.open, { reportFailure: false });
  const runClose = useAtomCommand(deviceEnvironment.close, { reportFailure: false });
  const runRotate = useAtomCommand(deviceEnvironment.rotate, { reportFailure: false });
  const runInput = useAtomCommand(deviceEnvironment.input, { reportFailure: false });

  const [followedDeviceId, setFollowedDeviceId] = useState<string | null>(null);
  const [pickedImage, setPickedImage] = useState<DeviceImageSelection | null>(null);
  const [startPending, setStartPending] = useState(false);
  const [actionError, setActionError] = useState<PaneError | null>(null);
  const [inputError, setInputError] = useState<InputErrorNote | null>(null);
  const [stillImages, setStillImages] = useState<StillImages | null>(null);
  const [pasteNote, setPasteNote] = useState<PasteNote | null>(null);
  const [rotateNote, setRotateNote] = useState<RotateNote | null>(null);
  // The devices with a rotate in flight. A second press would aim from the orientation the
  // catalog showed before that turn, so Rotate waits for the call to settle. The ref gates a
  // press at once, even before the buttons render held; the state is what they render from.
  const rotatingRef = useRef(new Set<string>());
  const [rotatingDeviceIds, setRotatingDeviceIds] = useState<ReadonlySet<string>>(() => new Set());
  const [lastTurn, setLastTurn] = useState<LastTurn | null>(null);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // The rotate note goes by itself; a newer note restarts the clock.
  useEffect(() => {
    if (rotateNote === null) return;
    const timer = setTimeout(() => {
      setRotateNote((current) => (current === rotateNote ? null : current));
    }, ROTATE_NOTE_MS);
    return () => clearTimeout(timer);
  }, [rotateNote]);

  // Memoized so the compiler treats them as settled values for the callbacks that use them.
  const selectedImage = useMemo(
    () => selectedImageOf(catalog, pickedImage, followedDeviceId),
    [catalog, pickedImage, followedDeviceId],
  );
  const device = useMemo(
    () => selectShownDevice(catalog, selectedImage, followedDeviceId),
    [catalog, selectedImage, followedDeviceId],
  );
  const selectImage = useCallback((platform: DevicePlatform, imageId: string) => {
    setPickedImage({ platform, imageId });
  }, []);

  const reportFailure = useCallback(
    (
      result: AtomCommandResult<unknown, unknown>,
      fallback: string,
      platform: DevicePlatform | null = null,
    ) => {
      if (result._tag !== "Failure" || !mountedRef.current || isAtomCommandInterrupted(result)) {
        return;
      }
      setActionError({
        ...describeDeviceError(squashAtomCommandFailure(result), fallback),
        platform,
      });
    },
    [],
  );

  // One input queue per device, made on first use and dropped with the device or the pane.
  const laneRef = useRef<InputLane | null>(null);
  const inputErrorRef = useRef<InputErrorNote | null>(null);
  const inputErrorTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(inputErrorTimerRef.current), []);
  // A refusal stays up for a few seconds after the latest one, so input accepted right after
  // it (the next keystroke) does not clear it before it can be read; a newer one replaces it.
  // Only a change re-renders: input runs at frame rate while dragging.
  const showInputError = useCallback((deviceId: string, message: string) => {
    const current = inputErrorRef.current;
    if (current?.deviceId !== deviceId || current.message !== message) {
      const note = { deviceId, message };
      inputErrorRef.current = note;
      setInputError(note);
    }
    clearTimeout(inputErrorTimerRef.current);
    inputErrorTimerRef.current = setTimeout(() => {
      inputErrorRef.current = null;
      setInputError(null);
    }, INPUT_ERROR_MS);
  }, []);
  const shownDeviceId = device?.deviceId ?? null;
  // The catalog is the fresher report once it moves off what it showed when the last turn was
  // pressed (it caught up, or the device turned by itself), and a result belongs to one device.
  if (
    lastTurn !== null &&
    (lastTurn.deviceId !== shownDeviceId || lastTurn.catalogOrientation !== device?.orientation)
  ) {
    setLastTurn(null);
  }
  useEffect(() => {
    const deviceId = shownDeviceId;
    return () => {
      const lane = laneRef.current;
      if (lane === null || lane.deviceId !== deviceId) return;
      lane.alive = false;
      lane.queue.dispose();
      laneRef.current = null;
    };
  }, [shownDeviceId]);
  const pushInput = useCallback(
    (event: DeviceInputEvent) => {
      if (shownDeviceId === null) return;
      let lane = laneRef.current;
      if (lane === null || lane.deviceId !== shownDeviceId) {
        if (lane !== null) {
          lane.alive = false;
          lane.queue.dispose();
        }
        const deviceId = shownDeviceId;
        const next: InputLane = {
          deviceId,
          alive: true,
          // A refused batch rejects, after its error is shown, so the queue sends its
          // releases again instead of leaving a key or a touch down on the device.
          queue: new DeviceInputQueue(async (events) => {
            const result = await runInput({ environmentId, input: { deviceId, events } });
            if (next.alive && result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
              showInputError(
                deviceId,
                describeDeviceError(
                  squashAtomCommandFailure(result),
                  "The device did not accept that input. Try again.",
                ).message,
              );
            }
            if (result._tag === "Failure") {
              throw new Error("device.input failed", { cause: squashAtomCommandFailure(result) });
            }
          }),
        };
        laneRef.current = next;
        lane = next;
      }
      lane.queue.push(event);
    },
    [environmentId, runInput, shownDeviceId, showInputError],
  );

  // Text an iOS simulator cannot type is pasted, and iOS may hold it behind its "Allow Paste"
  // prompt; said once per simulator.
  const shownPlatform = device?.platform ?? null;
  const notePaste = useCallback(() => {
    if (shownPlatform !== "ios" || shownDeviceId === null) return;
    setPasteNote((current) =>
      current?.deviceId === shownDeviceId ? current : { deviceId: shownDeviceId, open: true },
    );
  }, [shownDeviceId, shownPlatform]);
  const sendInput = useCallback(
    (event: DeviceInputEvent) => {
      pushInput(event);
      if (event.type === "text" && !isUsTypeable(event.text)) notePaste();
    },
    [notePaste, pushInput],
  );
  // The device shown now, for work that finishes after an await.
  const shownDeviceIdRef = useRef(shownDeviceId);
  useEffect(() => {
    shownDeviceIdRef.current = shownDeviceId;
  }, [shownDeviceId]);

  const releaseInput = useCallback((scope: "keys" | "all") => {
    const queue = laneRef.current?.queue;
    if (scope === "keys") queue?.releaseKeys();
    else queue?.releaseAll();
  }, []);

  const start = async () => {
    if (selectedImage === null) return;
    const { platform, imageId } = selectedImage;
    setStartPending(true);
    setActionError(null);
    const result = await runOpen({ environmentId, input: { platform, imageId } });
    if (!mountedRef.current) return;
    setStartPending(false);
    if (result._tag === "Success") {
      // The pick stays on the started image: until the catalog lists the new device, the
      // fallbacks would select another platform's device or the first image.
      setFollowedDeviceId(result.value.deviceId);
      setPickedImage({ platform, imageId });
      return;
    }
    reportFailure(result, "Could not start the device. Try again.", platform);
  };
  const stop = async (deviceId: string) => {
    setActionError(null);
    const result = await runClose({ environmentId, input: { deviceId } });
    if (result._tag === "Failure") reportFailure(result, "Could not stop the device. Try again.");
  };
  const rotate = async (direction: RotateDirection) => {
    if (device === null || !device.canRotate) return;
    const { deviceId } = device;
    if (rotatingRef.current.has(deviceId)) return;
    // The last turn's result is fresher than a catalog that has not reported that turn yet.
    const current = lastTurn?.deviceId === deviceId ? lastTurn.orientation : device.orientation;
    const orientation = rotatedOrientation(current, direction);
    setRotateNote(null);
    rotatingRef.current.add(deviceId);
    setRotatingDeviceIds(new Set(rotatingRef.current));
    try {
      const result = await runRotate({ environmentId, input: { deviceId, orientation } });
      if (result._tag === "Failure") {
        reportFailure(result, "Could not rotate the device. Try again.");
        return;
      }
      setLastTurn({
        deviceId,
        orientation: result.value.orientation,
        catalogOrientation: device.orientation,
      });
      // The device ended as it was: say so, rather than leave the press looking ignored.
      if (mountedRef.current && result.value.orientation !== orientation) {
        setRotateNote({ deviceId });
      }
    } finally {
      rotatingRef.current.delete(deviceId);
      setRotatingDeviceIds(new Set(rotatingRef.current));
    }
  };
  const pressKey = (key: DeviceKey) => {
    pushInput({ type: "key", key, action: "down" });
    pushInput({ type: "key", key, action: "up" });
  };
  const pasteClipboard = async () => {
    const target = shownDeviceId;
    let text: string;
    try {
      // Absent outside secure contexts; browsers may also ask, or refuse.
      text = await navigator.clipboard.readText();
    } catch {
      if (mountedRef.current && target !== null) {
        showInputError(target, CLIPBOARD_UNREADABLE_MESSAGE);
      }
      return;
    }
    // Reading can wait on the browser's own Paste prompt; the text belongs to that device only.
    if (!mountedRef.current || text.length === 0 || shownDeviceIdRef.current !== target) return;
    for (const part of splitDeviceText(text)) sendInput({ type: "text", text: part });
  };

  if (catalog === null) {
    const retry = <RetryButton retrying={catalogQuery.isPending} onRetry={catalogQuery.refresh} />;
    // The setting says on, so a server still saying "off" is catching up (or about to say
    // off here too); the catalog is read again once.
    if (disabledFailure) {
      return (
        <PanelMessage action={retry}>
          Waiting for the server to apply the Device previews setting…
        </PanelMessage>
      );
    }
    if (catalogQuery.error !== null) {
      return (
        <PanelMessage title="Could not load devices" action={retry}>
          {catalogQuery.error}
        </PanelMessage>
      );
    }
    return <PanelMessage>Looking for devices…</PanelMessage>;
  }

  // With no platform able to open a device and nothing running, each platform says why.
  if (!catalog.platforms.some(opensImages) && !catalog.devices.some(isRunning)) {
    return <UnavailablePlatforms platforms={catalog.platforms} />;
  }

  const shownAvailability =
    device === null
      ? null
      : (catalog.platforms.find((listed) => listed.platform === device.platform)?.availability ??
        null);
  const inputDisabledReason =
    shownAvailability !== null && isViewOnlyAvailability(shownAvailability.kind)
      ? shownAvailability.message
      : null;
  const shownName =
    device === null ? null : imageDisplayName(catalog, device.platform, device.imageId);
  // One device runs per platform; another image on that platform waits for it to stop.
  const platformDevice =
    selectedImage === null
      ? null
      : (catalog.devices.find(
          (candidate) => isRunning(candidate) && candidate.platform === selectedImage.platform,
        ) ?? null);
  const androidOnly = catalog.platforms.every((listed) => listed.platform === "android");
  const startDisabledReason =
    selectedImage === null
      ? androidOnly
        ? DEVICE_DISABLED_REASONS.noAvd
        : DEVICE_DISABLED_REASONS.noImage
      : startPending
        ? DEVICE_DISABLED_REASONS.starting
        : platformDevice === null
          ? null
          : platformDevice.state === "stopping"
            ? DEVICE_DISABLED_REASONS.stopping
            : DEVICE_DISABLED_REASONS.running;
  const otherDevice = device === null ? platformDevice : null;
  const otherName =
    otherDevice === null
      ? null
      : imageDisplayName(catalog, otherDevice.platform, otherDevice.imageId);

  // A device limit names the running device by display name, which may not match the catalog;
  // the device to stop is the one running on the platform the start was for.
  const runningName = actionError?.runningName ?? null;
  const limitPlatform = actionError?.platform ?? null;
  const limitDevice =
    runningName === null || limitPlatform === null
      ? null
      : (catalog.devices.find(
          (candidate) => isRunning(candidate) && candidate.platform === limitPlatform,
        ) ?? null);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <DeviceToolbar
        platforms={catalog.platforms}
        selectedImage={selectedImage}
        selectedName={
          selectedImage === null
            ? null
            : imageDisplayName(catalog, selectedImage.platform, selectedImage.imageId)
        }
        device={device}
        startDisabledReason={startDisabledReason}
        pickerLocked={startPending}
        viewOnly={inputDisabledReason !== null}
        rotating={device !== null && rotatingDeviceIds.has(device.deviceId)}
        onSelectImage={selectImage}
        onStart={() => void start()}
        onStop={() => {
          if (device !== null) void stop(device.deviceId);
        }}
        onRotate={(direction) => void rotate(direction)}
        onKey={pressKey}
        onPaste={() => void pasteClipboard()}
        stillImages={device?.state === "ready" ? stillImages : null}
      />
      {actionError === null ? null : (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-2 border-b border-border/60 px-3 py-2 text-xs text-destructive"
        >
          <TriangleAlertIcon aria-hidden className="size-3.5 shrink-0" />
          <span className="min-w-0 flex-1">{actionError.message}</span>
          {limitDevice === null ? null : (
            <Button size="xs" variant="outline" onClick={() => void stop(limitDevice.deviceId)}>
              Stop {runningName}
            </Button>
          )}
          {actionError.disabled ? <OpenSettingsButton environmentId={environmentId} /> : null}
          <Button size="xs" variant="ghost" onClick={() => setActionError(null)}>
            Dismiss
          </Button>
        </div>
      )}
      {inputError === null ||
      inputError.deviceId !== device?.deviceId ||
      device.state !== "ready" ? null : (
        <div
          role="status"
          data-input-error
          className="border-b border-border/60 px-3 py-1.5 text-xs text-foreground"
        >
          {inputError.message}
        </div>
      )}
      {rotateNote === null ||
      rotateNote.deviceId !== device?.deviceId ||
      device.state !== "ready" ? null : (
        <div
          role="status"
          data-rotate-note
          className="border-b border-border/60 px-3 py-1.5 text-xs text-foreground"
        >
          {rotateKeptNote(shownName ?? device.imageId)}
        </div>
      )}
      {pasteNote?.open !== true ||
      pasteNote.deviceId !== device?.deviceId ||
      device.state !== "ready" ? null : (
        <div
          role="status"
          data-paste-note
          className="flex flex-wrap items-center gap-2 border-b border-border/60 px-3 py-1.5 text-xs text-foreground"
        >
          <span className="min-w-0 flex-1">{IOS_PASTE_NOTE}</span>
          <Button
            size="xs"
            variant="ghost"
            onClick={() => setPasteNote({ deviceId: pasteNote.deviceId, open: false })}
          >
            Dismiss
          </Button>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col bg-muted/30 p-4">
        {device?.state === "ready" ? (
          <DeviceScreen
            key={device.deviceId}
            environmentId={environmentId}
            device={device}
            name={shownName ?? device.imageId}
            inputDisabledReason={inputDisabledReason}
            onInput={sendInput}
            onRelease={releaseInput}
            onStillImages={setStillImages}
          />
        ) : device === null ? (
          otherDevice !== null && otherName !== null ? (
            <PanelMessage
              action={
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => setPickedImage(selectionOf(otherDevice))}
                >
                  Show {otherName}
                </Button>
              }
            >
              {otherName} is running. Stop it to start another device.
            </PanelMessage>
          ) : (
            <PanelMessage>
              {selectedImage !== null
                ? "Pick a device and press Start."
                : androidOnly
                  ? "Create a virtual device in Android Studio's Device Manager, then come back."
                  : "No device can start on this server yet."}
            </PanelMessage>
          )
        ) : device.state === "failed" ? (
          <PanelMessage title={deviceStateLabel(device)}>
            Press Start to try again, or pick another device.
          </PanelMessage>
        ) : device.state === "stopping" ? (
          <PanelMessage>Stopping {shownName}…</PanelMessage>
        ) : (
          <PanelMessage>Starting {shownName}. The first boot can take a few minutes.</PanelMessage>
        )}
      </div>
    </div>
  );
}
