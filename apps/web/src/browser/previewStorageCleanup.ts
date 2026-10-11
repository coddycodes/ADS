import type { EnvironmentId } from "@bibcode/contracts";

/**
 * Deletes a removed environment's preview storage on the desktop. Best effort:
 * a failure leaves stale storage on disk and never fails the removal itself.
 * Reads the bridge live: this runs from connection setup, which loads before
 * the desktop bridge is installed.
 */
export async function forgetPreviewStorage(environmentId: EnvironmentId): Promise<void> {
  const preview = typeof window === "undefined" ? undefined : window.desktopBridge?.preview;
  await preview?.forgetEnvironment(environmentId).catch(() => undefined);
}
