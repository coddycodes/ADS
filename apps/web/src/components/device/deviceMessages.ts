import type { DeviceAvailabilityKind, DeviceSnapshot } from "@bibcode/contracts";
import { isQueryTransportCutoff } from "@bibcode/client-runtime/state/runtime";

/** The Settings section the pane's "Open settings" lands on. */
export const DEVICE_PREVIEWS_SETTINGS_ANCHOR = "device-previews";

/** Shown wherever device previews are off, with a way to Settings next to it. */
export const DEVICE_PREVIEWS_OFF_MESSAGE =
  "Device previews are off. Turn them on in Settings → Device previews.";

export const DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE =
  "This server's BiBCode version has no device previews. Update the server to use them.";

export const DEVICE_PREVIEWS_DESCRIPTION =
  "Run an Android emulator on this environment's server and show it in a side panel. Requires the Android SDK with at least one virtual device and hardware acceleration on the server.";

/** Why a toolbar control is disabled; every disabled control says one of these. */
export const DEVICE_DISABLED_REASONS = {
  startFirst: "Start the device first",
  notBooted: "Available once the device has booted",
  stopping: "The device is stopping",
  running: "Stop the running device to start another",
  noAvd: "Create a virtual device in Android Studio's Device Manager first",
  starting: "The device is starting",
} as const;

const AVAILABILITY_TITLES: Readonly<Record<DeviceAvailabilityKind, string>> = {
  ready: "Android emulator ready",
  sdkMissing: "Android SDK not found",
  adbMissing: "Android platform-tools missing",
  noAvds: "No virtual devices",
  accelerationUnavailable: "Hardware acceleration unavailable",
};

/** The heading above the server's own explanation of why no device can start. */
export function availabilityTitle(kind: DeviceAvailabilityKind): string {
  return AVAILABILITY_TITLES[kind];
}

/** The state pill text, such as `Booting…` or `Failed: <reason>`. */
export function deviceStateLabel(device: DeviceSnapshot): string {
  switch (device.state) {
    case "starting":
      return "Starting…";
    case "booting":
      return "Booting…";
    case "ready":
      return "Ready";
    case "stopping":
      return "Stopping…";
    case "stopped":
      return "Stopped";
    case "failed":
      return `Failed: ${device.failure ?? "the emulator stopped unexpectedly"}`;
  }
}

/** A device RPC failure, as the pane shows it. */
export interface DeviceErrorView {
  readonly message: string;
  /** For a device limit: the AVD already running, which the pane offers to stop. */
  readonly runningAvd: string | null;
  /** Device previews were turned off meanwhile; the pane offers Settings. */
  readonly disabled: boolean;
}

const CONNECTION_DROPPED = "The connection to the server dropped. Try again once it reconnects.";

function taggedField(error: unknown, field: string): string | null {
  if (typeof error !== "object" || error === null) return null;
  const value = (error as Record<string, unknown>)[field];
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

/**
 * Maps a device RPC failure to actionable copy. The server's messages already say what to do
 * (set `ANDROID_HOME`, create an AVD, stop the running device, try again), so they are kept;
 * the pane adds the action button.
 */
export function describeDeviceError(error: unknown, fallback: string): DeviceErrorView {
  const tag = taggedField(error, "_tag");
  const message = taggedField(error, "message");
  const plain = (text: string): DeviceErrorView => ({
    message: text,
    runningAvd: null,
    disabled: false,
  });
  switch (tag) {
    case "DevicePreviewsDisabledError":
      return { message: DEVICE_PREVIEWS_OFF_MESSAGE, runningAvd: null, disabled: true };
    case "DeviceLimitError": {
      const running = taggedField(error, "running");
      return {
        message:
          running === null
            ? "Another device is already running. Stop it before starting another."
            : `${running} is already running. Stop it before starting another device.`,
        runningAvd: running,
        disabled: false,
      };
    }
    case "DeviceSdkUnavailableError":
      return plain(message ?? "The Android SDK is not available on this server.");
    case "DeviceNotFoundError":
      return plain(message ?? "This device is no longer running. Start it again.");
    case "DeviceNotReadyError":
      return plain(message ?? "The device is not ready yet. Try again in a moment.");
    case "DeviceInputError":
      return plain(message ?? "The device did not accept that input.");
    case "DeviceEmulatorError":
      return plain(
        message ??
          "The emulator did not respond. Try again; if it keeps failing, stop the device and start it again.",
      );
    default:
      if (isQueryTransportCutoff(error)) return plain(CONNECTION_DROPPED);
      return plain(fallback);
  }
}
