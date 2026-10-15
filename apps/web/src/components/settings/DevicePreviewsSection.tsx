import type { DeviceSnapshot, EnvironmentId } from "@bibcode/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import { InfoIcon, TriangleAlertIcon } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { useUpdateEnvironmentSettings } from "../../hooks/useSettings";
import { deviceEnvironment } from "../../state/device";
import { useEnvironment } from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { useDevicePreviewsAvailability } from "../device/deviceAvailability";
import {
  DEVICE_PREVIEWS_DESCRIPTION,
  DEVICE_PREVIEWS_SETTINGS_ANCHOR,
  DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE,
} from "../device/deviceMessages";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../ui/alert-dialog";
import { Button } from "../ui/button";
import { Switch } from "../ui/switch";
import { SettingsRow, SettingsSection } from "./settingsLayout";

const TOGGLE_TITLE = "Device previews (Android emulator)";

const isRunning = (device: DeviceSnapshot) =>
  device.state !== "stopped" && device.state !== "failed";

/** The `enableDevicePreviews` toggle for one environment's server. */
export function DevicePreviewsSection({
  environmentId,
}: {
  readonly environmentId: EnvironmentId | null;
}) {
  if (environmentId === null) return null;
  return (
    <SettingsSection title="Device previews" id={DEVICE_PREVIEWS_SETTINGS_ANCHOR}>
      <DevicePreviewsToggle key={environmentId} environmentId={environmentId} />
    </SettingsSection>
  );
}

function DevicePreviewsToggle({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const environment = useEnvironment(environmentId);
  const availability = useDevicePreviewsAvailability(environmentId);
  const updateSettings = useUpdateEnvironmentSettings(environmentId);
  const reasonId = useId();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  // The catalog is only served while previews are on; it names a device turning off would stop.
  const catalog = useEnvironmentQuery(
    availability.enabled ? deviceEnvironment.catalog({ environmentId, input: {} }) : null,
  ).data;
  const running = catalog?.devices.find(isRunning) ?? null;
  const label = environment?.label.trim() || "this server";

  if (!availability.known || !availability.supported) {
    const reason = availability.known
      ? DEVICE_PREVIEWS_UNSUPPORTED_MESSAGE
      : `Connecting to ${label}…`;
    return (
      <SettingsRow
        title={TOGGLE_TITLE}
        description={DEVICE_PREVIEWS_DESCRIPTION}
        status={
          <div id={reasonId} className="flex items-start gap-1.5">
            <InfoIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
            <span>{reason}</span>
          </div>
        }
        control={
          <Switch checked={false} disabled aria-label={TOGGLE_TITLE} aria-describedby={reasonId} />
        }
      />
    );
  }

  const setEnabled = async (enabled: boolean) => {
    setSaving(true);
    setSaveError(null);
    const result = await updateSettings({ enableDevicePreviews: enabled });
    if (!mountedRef.current) return;
    setSaving(false);
    if (result._tag !== "Failure" || isAtomCommandInterrupted(result)) return;
    const failure = squashAtomCommandFailure(result);
    const detail =
      failure instanceof Error && failure.message.trim().length > 0
        ? failure.message
        : `Check the connection to ${label}.`;
    setSaveError(`Could not turn device previews ${enabled ? "on" : "off"}. ${detail} Try again.`);
  };
  const handleToggle = (checked: boolean) => {
    // Turning off stops the running device, so that is confirmed first.
    if (!checked && running !== null) {
      setConfirmOpen(true);
      return;
    }
    void setEnabled(checked);
  };

  return (
    <>
      <SettingsRow
        title={TOGGLE_TITLE}
        description={DEVICE_PREVIEWS_DESCRIPTION}
        status={
          saving ? (
            "Saving…"
          ) : saveError !== null ? (
            <div role="alert" className="flex items-start gap-1.5 text-destructive">
              <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
              <span>{saveError}</span>
            </div>
          ) : running === null ? null : (
            `${running.avdName} is running on ${label}.`
          )
        }
        control={
          <Switch
            checked={availability.enabled}
            disabled={saving}
            onCheckedChange={handleToggle}
            aria-label={TOGGLE_TITLE}
          />
        }
      />
      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogPopup>
          <AlertDialogHeader>
            <AlertDialogTitle>Turn off device previews?</AlertDialogTitle>
            <AlertDialogDescription>
              This stops {running?.avdName ?? "the running device"} on {label}.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogClose render={<Button variant="outline" />}>Cancel</AlertDialogClose>
            <Button
              variant="destructive"
              onClick={() => {
                setConfirmOpen(false);
                void setEnabled(false);
              }}
            >
              Stop {running?.avdName ?? "device"} and turn off
            </Button>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </>
  );
}
