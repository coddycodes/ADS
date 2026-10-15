import type { EnvironmentId, ServerConfig } from "@bibcode/contracts";
import { useAtomValue } from "@effect/atom-react";

import { serverEnvironment } from "../../state/server";

/** Whether an environment offers device previews, from its server config. */
export interface DevicePreviewsAvailability {
  /** The server config has arrived; until then nothing is known. */
  readonly known: boolean;
  /** The server has device previews (the `devicePreviews` capability). */
  readonly supported: boolean;
  /** `enableDevicePreviews` is on. */
  readonly enabled: boolean;
}

export function devicePreviewsAvailability(
  serverConfig: ServerConfig | null | undefined,
): DevicePreviewsAvailability {
  if (serverConfig === null || serverConfig === undefined) {
    return { known: false, supported: false, enabled: false };
  }
  const supported = serverConfig.environment.capabilities.devicePreviews === true;
  return {
    known: true,
    supported,
    enabled: supported && serverConfig.settings.enableDevicePreviews === true,
  };
}

/** The one gate for every device-preview entry point: menu, palette, pane, and settings. */
export function useDevicePreviewsAvailability(
  environmentId: EnvironmentId | null,
): DevicePreviewsAvailability {
  return devicePreviewsAvailability(useAtomValue(serverEnvironment.configValueAtom(environmentId)));
}
