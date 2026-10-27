import { VcsCloneStoppedError } from "@coddy/client-runtime/state/vcs";
import type { EnvironmentId } from "@coddy/contracts";
import { GitCloneOperationError } from "@coddy/contracts";
import { isWindowsAbsolutePath } from "@coddy/shared/path";
import * as Schema from "effect/Schema";

import {
  ensureBrowseDirectoryPath,
  isUnsupportedWindowsProjectPath,
  normalizeProjectPathForDispatch,
} from "~/lib/projectPaths";

import { canUseNativeHostFolderPicker } from "../hostFolderPicker";
export { getEnvironmentBrowsePlatform } from "../hostFolderPicker";

export type AddProjectStep =
  | "start"
  | "host-path"
  | "remote-browse"
  | "clone"
  | "clone-parent-browse"
  | "create"
  | "create-parent-browse";

/**
 * `cloning` and `reconnecting` can be cancelled; `cancelling` waits for the server to confirm;
 * `registering` adds the finished clone as a project.
 */
export type AddProjectCloneProgress =
  | "idle"
  | "cloning"
  | "reconnecting"
  | "cancelling"
  | "registering";

const isVcsCloneStoppedError = Schema.is(VcsCloneStoppedError);
const isGitCloneOperationError = Schema.is(GitCloneOperationError);

export const CLONE_CANCELLED_NOTICE = "Clone cancelled.";
export const CLONE_STOPPED_ERROR = "The clone stopped before it finished. Try again.";

export function cloneReconnectingNotice(hostLabel: string): string {
  return `Lost the connection to ${hostLabel}. The clone continues on the server; reconnecting…`;
}

export function cloneCancelPendingNotice(hostLabel: string): string {
  return `The clone stops when ${hostLabel} reconnects.`;
}

/** Git finished before the Cancel reached the host: the clone is on disk and left unregistered. */
export function cloneFinishedBeforeCancelNotice(path: string): string {
  return `The clone finished before it could be cancelled. It is in ${path} and was not added as a project. Press Clone to add it.`;
}

export interface AddProjectCloneFeedback {
  readonly kind: "error" | "notice";
  readonly text: string;
}

/**
 * The dialog's copy for a clone the server's clone runtime refused or ended, or that the client
 * stopped following. `null` for any other failure, which keeps "Clone failed: <detail>".
 */
export function describeCloneOperationFailure(
  error: unknown,
  hostLabel: string,
  cancelRequested: boolean,
): AddProjectCloneFeedback | null {
  if (isVcsCloneStoppedError(error)) {
    return {
      kind: "error",
      text:
        error.reason === "environment-unavailable"
          ? `Can't reconnect to ${hostLabel}. The clone continues there; clone the same URL into the same folder to finish.`
          : CLONE_STOPPED_ERROR,
    };
  }
  if (!isGitCloneOperationError(error)) {
    return null;
  }
  const path = error.destination;
  switch (error.reason) {
    case "cancelled":
      return cancelRequested
        ? { kind: "notice", text: CLONE_CANCELLED_NOTICE }
        : {
            kind: "error",
            text: `The clone into ${path} was cancelled elsewhere. Press Clone to start again.`,
          };
    case "not-in-progress":
      return {
        kind: "error",
        text: `No clone is in progress for ${path}. Press Clone to start again.`,
      };
    case "busy":
      return {
        kind: "error",
        text: `Another clone into ${path} is in progress. Wait for it to finish or choose another folder.`,
      };
    case "capacity":
      return {
        kind: "error",
        text: `Too many clones are running on ${hostLabel}. Wait for one to finish and try again.`,
      };
    case "shutting-down":
      return {
        kind: "error",
        text: `${hostLabel} is shutting down. Press Clone again once it is back.`,
      };
  }
}

export interface AddProjectHostOption {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly platform: string | null;
  readonly baseDirectory: string;
  readonly isPrimary: boolean;
  readonly desktopInstanceId: string | null;
  readonly nativePickerAvailable: boolean;
}

export function defaultAddProjectParent(value: string | null | undefined): string {
  const trimmed = value?.trim() ?? "";
  return ensureBrowseDirectoryPath(trimmed.length === 0 ? "~/" : trimmed);
}

export function validateProjectName(value: string): string | null {
  const name = value.trim();
  if (name.length === 0) return "Enter a project name.";
  if (name === "." || name === "..") return "Enter a project name other than . or ..";
  if (name.includes("/") || name.includes("\\")) {
    return "Project names cannot contain path separators.";
  }
  return null;
}

export function validateAddProjectPath(value: string, platform: string | null): string | null {
  const path = value.trim();
  if (path.length === 0) return "Enter a project path.";
  if (platform === null) return "Host platform information is still loading.";
  if (isUnsupportedWindowsProjectPath(path, platform)) {
    return "Windows-style paths are only supported on Windows.";
  }
  if (
    path === "~" ||
    path.startsWith("~/") ||
    path.startsWith("~\\") ||
    path.startsWith("/") ||
    isWindowsAbsolutePath(path)
  ) {
    return null;
  }
  return "Enter an absolute or home-relative path.";
}

const supportedGitCloneProtocols = new Set(["http:", "https:", "ssh:", "git:"]);
const scpStyleGitCloneUrlPattern = /^(?:[^@\s/:]+@)?[^@:\s/]+:[^\s]+$/;

export function validateGitCloneUrl(value: string): string | null {
  const url = value.trim();
  if (url.length === 0) return "Enter a Git URL.";
  if (/\s/.test(url)) return "Enter a valid Git repository URL.";
  if (isWindowsAbsolutePath(url)) return "Enter a valid Git repository URL.";
  if (!url.includes("://") && scpStyleGitCloneUrlPattern.test(url)) return null;

  try {
    const parsed = new URL(url);
    if (
      supportedGitCloneProtocols.has(parsed.protocol) &&
      parsed.hostname.length > 0 &&
      parsed.pathname.length > 1
    ) {
      return null;
    }
  } catch {
    // The shared validation result below covers malformed URLs.
  }
  return "Enter a valid Git repository URL.";
}

export function validateGitCloneParentPath(value: string, platform: string | null): string | null {
  const path = value.trim();
  if (path.length === 0) return "Enter a clone parent folder.";
  if (platform === null) return "Host platform information is still loading.";
  if (isUnsupportedWindowsProjectPath(path, platform)) {
    return "Windows-style paths are only supported on Windows.";
  }
  if (
    path === "~" ||
    path.startsWith("~/") ||
    path.startsWith("~\\") ||
    path.startsWith("/") ||
    isWindowsAbsolutePath(path)
  ) {
    return null;
  }
  return "Enter an absolute or home-relative path.";
}

export function joinProjectPath(parent: string, name: string, platform: string): string {
  const normalizedParent = normalizeProjectPathForDispatch(parent);
  const separator = /^win(dows|32)?/i.test(platform) ? "\\" : "/";
  return `${normalizedParent}${normalizedParent.endsWith(separator) ? "" : separator}${name.trim()}`;
}

export function shouldUseNativePicker(host: AddProjectHostOption): boolean {
  return canUseNativeHostFolderPicker(host);
}
