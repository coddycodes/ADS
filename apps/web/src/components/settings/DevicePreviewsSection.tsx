import type { DeviceSnapshot, EnvironmentId } from "@bibcode/contracts";
import { InfoIcon } from "lucide-react";
import { useId, useState } from "react";

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

  const setEnabled = (enabled: boolean) => {
    void updateSettings({ enableDevicePreviews: enabled });
  };
  const handleToggle = (checked: boolean) => {
    // Turning off stops the running device, so that is confirmed first.
    if (!checked && running !== null) {
      setConfirmOpen(true);
      return;
    }
    setEnabled(checked);
  };

  return (
    <>
      <SettingsRow
        title={TOGGLE_TITLE}
        description={DEVICE_PREVIEWS_DESCRIPTION}
        status={running === null ? null : `${running.avdName} is running on ${label}.`}
        control={
          <Switch
            checked={availability.enabled}
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
                setEnabled(false);
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
