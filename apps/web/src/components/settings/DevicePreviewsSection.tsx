import type { DeviceCatalog, DeviceSnapshot, EnvironmentId } from "@coddy/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@coddy/client-runtime/state/runtime";
import { InfoIcon, TriangleAlertIcon } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { useUpdateEnvironmentSettings } from "../../hooks/useSettings";
import { deviceEnvironment } from "../../state/device";
import { useEnvironment, useEnvironments, usePrimaryEnvironment } from "../../state/environments";
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
import { SettingsPageContainer, SettingsRow, SettingsSection } from "./settingsLayout";

const TOGGLE_TITLE = "Device previews";

const isRunning = (device: DeviceSnapshot) =>
  device.state !== "stopped" && device.state !== "failed";

/** The running devices' display names (a simulator's id is a UDID nobody recognizes). */
function runningNames(catalog: DeviceCatalog | null | undefined): ReadonlyArray<string> {
  if (catalog === null || catalog === undefined) return [];
  return catalog.devices
    .filter(isRunning)
    .map(
      (device) =>
        catalog.platforms
          .find((listed) => listed.platform === device.platform)
          ?.images.find((image) => image.id === device.imageId)?.displayName ?? device.imageId,
    );
}

const joinNames = (names: ReadonlyArray<string>) =>
  names.length <= 1 ? (names[0] ?? "") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/** Settings → Mobile emulators: the device-preview toggle of every connected environment. */
export function MobileSettingsPanel() {
  const { environments } = useEnvironments();
  const primaryEnvironment = usePrimaryEnvironment();
  const connected = environments.filter((candidate) => candidate.connection.phase === "connected");
  // Until an environment connects, the primary one shows why its toggle can't be used yet.
  const shown =
    connected.length > 0 ? connected : primaryEnvironment === null ? [] : [primaryEnvironment];
  return (
    <SettingsPageContainer>
      {shown.map((environment) => (
        <DevicePreviewsSection
          key={environment.environmentId}
          environmentId={environment.environmentId}
          title={shown.length > 1 ? `Device previews · ${environment.label}` : undefined}
        />
      ))}
    </SettingsPageContainer>
  );
}

/** The `enableDevicePreviews` toggle for one environment's server. */
export function DevicePreviewsSection({
  environmentId,
  title = "Device previews",
}: {
  readonly environmentId: EnvironmentId | null;
  /** Names the environment when the page lists several. */
  readonly title?: string | undefined;
}) {
  if (environmentId === null) return null;
  return (
    <SettingsSection title={title} id={`${DEVICE_PREVIEWS_SETTINGS_ANCHOR}-${environmentId}`}>
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
  const running = runningNames(catalog);
  const runningList = joinNames(running);
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
    if (!checked && running.length > 0) {
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
          ) : running.length === 0 ? null : (
            `${runningList} ${running.length === 1 ? "is" : "are"} running on ${label}.`
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
              This stops {runningList || "the running device"} on {label}.
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
              {running.length === 1
                ? `Stop ${runningList} and turn off`
                : "Stop devices and turn off"}
            </Button>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </>
  );
}
