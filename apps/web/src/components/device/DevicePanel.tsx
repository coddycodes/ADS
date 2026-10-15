import type {
  DeviceCatalog,
  DeviceInputEvent,
  DeviceKey,
  DeviceOrientation,
  DeviceSnapshot,
  EnvironmentId,
} from "@bibcode/contracts";
import {
  type AtomCommandResult,
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import { useNavigate } from "@tanstack/react-router";
import * as Cause from "effect/Cause";
import { SmartphoneIcon, TriangleAlertIcon } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";

import { deviceEnvironment } from "../../state/device";
import { useEnvironmentQuery } from "../../state/query";
import { useAtomCommand } from "../../state/use-atom-command";
import { Button } from "../ui/button";
import { RetryButton } from "../ui/retry-button";
import { useDevicePreviewsAvailability } from "./deviceAvailability";
import { DeviceInputQueue } from "./deviceInputQueue";
import {
  availabilityTitle,
  DEVICE_PREVIEWS_OFF_MESSAGE,
  DEVICE_PREVIEWS_SETTINGS_ANCHOR,
  DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE,
  type DeviceErrorView,
  describeDeviceError,
  deviceStateLabel,
  type StillImagesReason,
} from "./deviceMessages";
import { DeviceScreen } from "./DeviceScreen";
import { DeviceToolbar, type RotateDirection } from "./DeviceToolbar";

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

const isRunning = (device: DeviceSnapshot) =>
  device.state !== "stopped" && device.state !== "failed";

/**
 * The device the pane shows: the environment's running one (there is at most one), else the
 * one this pane last started, so its failure reason stays visible.
 */
export function selectShownDevice(
  catalog: DeviceCatalog | null,
  followedDeviceId: string | null,
): DeviceSnapshot | null {
  if (catalog === null) return null;
  return (
    catalog.devices.find(isRunning) ??
    catalog.devices.find(
      (device) => device.deviceId === followedDeviceId && device.state === "failed",
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
          to: "/settings/diagnostics",
          search: { environment: environmentId },
          hash: DEVICE_PREVIEWS_SETTINGS_ANCHOR,
        });
      }}
    >
      Open settings
    </Button>
  );
}

/** The right-panel device pane for one environment's Android emulator. */
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
  const [pickedAvd, setPickedAvd] = useState<string | null>(null);
  const [startPending, setStartPending] = useState(false);
  const [actionError, setActionError] = useState<DeviceErrorView | null>(null);
  const [inputError, setInputError] = useState<string | null>(null);
  const [stillImagesReason, setStillImagesReason] = useState<StillImagesReason | null>(null);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const device = selectShownDevice(catalog, followedDeviceId);
  const avds = catalog?.avds ?? [];
  const selectedAvd =
    device !== null && isRunning(device)
      ? device.avdName
      : pickedAvd !== null && avds.some((avd) => avd.name === pickedAvd)
        ? pickedAvd
        : (device?.avdName ?? avds[0]?.name ?? null);

  const reportFailure = useCallback(
    (result: AtomCommandResult<unknown, unknown>, fallback: string) => {
      if (result._tag !== "Failure" || !mountedRef.current || isAtomCommandInterrupted(result)) {
        return;
      }
      setActionError(describeDeviceError(squashAtomCommandFailure(result), fallback));
    },
    [],
  );

  // One input queue per device, made on first use and dropped with the device or the pane.
  const laneRef = useRef<InputLane | null>(null);
  const inputErrorRef = useRef<string | null>(null);
  const shownDeviceId = device?.deviceId ?? null;
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
          queue: new DeviceInputQueue(async (events) => {
            const result = await runInput({ environmentId, input: { deviceId, events } });
            if (!next.alive) return;
            const message =
              result._tag === "Failure" && !isAtomCommandInterrupted(result)
                ? describeDeviceError(
                    squashAtomCommandFailure(result),
                    "The device did not accept that input. Try again.",
                  ).message
                : null;
            // Only a change re-renders: input runs at frame rate while dragging.
            if (message !== inputErrorRef.current) {
              inputErrorRef.current = message;
              setInputError(message);
            }
          }),
        };
        laneRef.current = next;
        lane = next;
      }
      lane.queue.push(event);
    },
    [environmentId, runInput, shownDeviceId],
  );

  const releaseInput = useCallback((scope: "keys" | "all") => {
    const queue = laneRef.current?.queue;
    if (scope === "keys") queue?.releaseKeys();
    else queue?.releaseAll();
  }, []);

  const start = async () => {
    if (selectedAvd === null) return;
    setStartPending(true);
    setActionError(null);
    const result = await runOpen({ environmentId, input: { avdName: selectedAvd } });
    if (!mountedRef.current) return;
    setStartPending(false);
    if (result._tag === "Success") {
      setFollowedDeviceId(result.value.deviceId);
      return;
    }
    reportFailure(result, "Could not start the device. Try again.");
  };
  const stop = async (deviceId: string) => {
    setActionError(null);
    const result = await runClose({ environmentId, input: { deviceId } });
    if (result._tag === "Failure") reportFailure(result, "Could not stop the device. Try again.");
  };
  const rotate = async (direction: RotateDirection) => {
    if (device === null) return;
    const result = await runRotate({
      environmentId,
      input: {
        deviceId: device.deviceId,
        orientation: rotatedOrientation(device.orientation, direction),
      },
    });
    if (result._tag === "Failure") reportFailure(result, "Could not rotate the device. Try again.");
  };
  const pressKey = (key: DeviceKey) => {
    pushInput({ type: "key", key, action: "down" });
    pushInput({ type: "key", key, action: "up" });
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

  if (catalog.availability.kind !== "ready" && device === null) {
    return (
      <PanelMessage title={availabilityTitle(catalog.availability.kind)}>
        {catalog.availability.message}
      </PanelMessage>
    );
  }

  const limitDevice =
    actionError === null || actionError.runningAvd === null
      ? null
      : (catalog.devices.find(
          (candidate) => candidate.avdName === actionError.runningAvd && isRunning(candidate),
        ) ?? null);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <DeviceToolbar
        avds={avds}
        selectedAvd={selectedAvd}
        device={device}
        startPending={startPending}
        onSelectAvd={setPickedAvd}
        onStart={() => void start()}
        onStop={() => {
          if (device !== null) void stop(device.deviceId);
        }}
        onRotate={(direction) => void rotate(direction)}
        onKey={pressKey}
        stillImagesReason={device?.state === "ready" ? stillImagesReason : null}
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
              Stop {limitDevice.avdName}
            </Button>
          )}
          {actionError.disabled ? <OpenSettingsButton environmentId={environmentId} /> : null}
          <Button size="xs" variant="ghost" onClick={() => setActionError(null)}>
            Dismiss
          </Button>
        </div>
      )}
      {inputError === null || device?.state !== "ready" ? null : (
        <div
          role="status"
          className="border-b border-border/60 px-3 py-1.5 text-xs text-foreground"
        >
          {inputError}
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col bg-muted/30 p-4">
        {device?.state === "ready" ? (
          <DeviceScreen
            key={device.deviceId}
            environmentId={environmentId}
            device={device}
            onInput={pushInput}
            onRelease={releaseInput}
            onStillImages={setStillImagesReason}
          />
        ) : device === null ? (
          <PanelMessage>
            {avds.length === 0
              ? "Create a virtual device in Android Studio's Device Manager, then come back."
              : "Pick a virtual device and press Start."}
          </PanelMessage>
        ) : device.state === "failed" ? (
          <PanelMessage title={deviceStateLabel(device)}>
            Press Start to try again, or pick another virtual device.
          </PanelMessage>
        ) : device.state === "stopping" ? (
          <PanelMessage>Stopping {device.avdName}…</PanelMessage>
        ) : (
          <PanelMessage>
            Starting {device.avdName}. The first boot can take a few minutes.
          </PanelMessage>
        )}
      </div>
    </div>
  );
}
