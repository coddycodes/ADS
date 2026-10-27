import { useEffect, useRef } from "react";
import { useDesktopUpdateState } from "../state/desktopUpdate";
import { stackedThreadToast, toastManager } from "./ui/toast";

export function remoteUpdateRequestToast(
  requestedBy: { readonly label: string; readonly detail: string | null },
  version: string | null,
) {
  const device =
    requestedBy.detail === null
      ? requestedBy.label
      : `${requestedBy.label} on ${requestedBy.detail}`;
  return {
    title: "Update requested from another device",
    description: `${device} is installing ${version === null ? "an update" : `v${version}`}. Coddy Studio restarts when it's done.`,
  };
}

/** Host notice stays informational; remote installs are already authorized by the requesting client. */
export function RemoteUpdateRequestNotifier({
  onManageDevices,
}: {
  readonly onManageDevices: () => void;
}) {
  const state = useDesktopUpdateState();
  const requestedBy = state?.requestedBy ?? null;
  const announced = useRef(false);
  useEffect(() => {
    if (requestedBy === null) {
      announced.current = false;
      return;
    }
    if (announced.current) return;
    announced.current = true;
    toastManager.add(
      stackedThreadToast({
        type: "info",
        ...remoteUpdateRequestToast(
          requestedBy,
          state?.downloadedVersion ?? state?.availableVersion ?? null,
        ),
        actionVariant: "outline",
        actionProps: { children: "Manage devices", onClick: onManageDevices },
      }),
    );
  }, [onManageDevices, requestedBy, state?.downloadedVersion, state?.availableVersion]);
  return null;
}
