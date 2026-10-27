import type { DesktopWslState, EnvironmentId, LocalApi } from "@coddy/contracts";
import { PRIMARY_LOCAL_ENVIRONMENT_ID } from "@coddy/contracts";

import {
  applyWslEnvironmentConfiguration,
  parseWslUncPath,
  resolveProjectPickerTarget,
  resolveWslProjectSelection,
  type WslEnvironmentCandidate,
} from "~/wslPaths";

export interface HostFolderPickerTarget {
  readonly environmentId: EnvironmentId;
  readonly platform: string | null;
  readonly isPrimary: boolean;
  readonly desktopInstanceId: string | null;
  readonly nativePickerAvailable: boolean;
}

export type PickHostFolderResult =
  | { readonly _tag: "Cancelled" }
  | { readonly _tag: "Selected"; readonly environmentId: EnvironmentId; readonly path: string }
  | { readonly _tag: "Failure"; readonly message: string };

export interface PickHostFolderInput {
  readonly host: HostFolderPickerTarget;
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly initialPath: string;
  readonly dialogs: Pick<LocalApi["dialogs"], "pickFolder">;
  readonly getWslState: () => Promise<DesktopWslState | null>;
  readonly primaryRunningDistro: string | null;
  readonly wslCandidates: ReadonlyArray<WslEnvironmentCandidate<EnvironmentId>>;
}

export function canUseNativeHostFolderPicker(target: HostFolderPickerTarget): boolean {
  return target.nativePickerAvailable && (target.isPrimary || target.desktopInstanceId !== null);
}

export function getEnvironmentBrowsePlatform(os: string | null | undefined): string | null {
  if (os === "windows") return "Win32";
  if (os === "darwin") return "MacIntel";
  if (os === "linux") return "Linux";
  return null;
}

export function readPrimaryRunningDistro(): string | null {
  if (typeof window === "undefined" || window.desktopBridge === undefined) return null;
  try {
    return (
      window.desktopBridge
        .getLocalEnvironmentBootstraps()
        .find((entry) => entry.id === PRIMARY_LOCAL_ENVIRONMENT_ID)?.runningDistro ?? null
    );
  } catch {
    return null;
  }
}

export async function pickHostFolder(input: PickHostFolderInput): Promise<PickHostFolderResult> {
  if (!canUseNativeHostFolderPicker(input.host)) {
    return {
      _tag: "Failure",
      message: "This host does not support folder picking. Enter its project path manually.",
    };
  }

  const wslState =
    input.host.isPrimary && input.host.platform === "Linux"
      ? await input.getWslState().catch(() => null)
      : null;
  const targetEnvironmentId = resolveProjectPickerTarget({
    browseEnvironmentId: input.host.environmentId,
    primaryEnvironmentId: input.primaryEnvironmentId,
    desktopInstanceId: input.host.desktopInstanceId,
    wslConfiguration: wslState,
  });
  const pickedPath = await input.dialogs.pickFolder({
    initialPath: input.initialPath,
    ...(targetEnvironmentId ? { targetEnvironmentId } : {}),
  });
  if (!pickedPath) return { _tag: "Cancelled" };
  if (!parseWslUncPath(pickedPath)) {
    return {
      _tag: "Selected",
      environmentId: input.host.environmentId,
      path: pickedPath,
    };
  }

  const selection = resolveWslProjectSelection(
    pickedPath,
    applyWslEnvironmentConfiguration(
      input.wslCandidates,
      input.primaryEnvironmentId,
      wslState,
      input.primaryRunningDistro,
    ),
  );
  return selection
    ? { _tag: "Selected", environmentId: selection.environmentId, path: selection.linuxPath }
    : {
        _tag: "Failure",
        message: "Start the matching WSL backend, then choose the folder again.",
      };
}
