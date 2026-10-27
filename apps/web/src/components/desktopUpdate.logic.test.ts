import { describe, expect, it } from "vite-plus/test";
import type { DesktopUpdateActionResult, DesktopUpdateState } from "@coddy/contracts";

import {
  canCheckForUpdate,
  getArm64IntelBuildWarningDescription,
  getDesktopUpdateActionError,
  getDesktopUpdateButtonTooltip,
  getDesktopBackendRecoveryMessage,
  getDesktopUpdateInstallConfirmationMessage,
  isDesktopUpdateButtonDisabled,
  resolveDesktopUpdateButtonAction,
  shouldHighlightDesktopUpdateError,
  shouldShowArm64IntelBuildWarning,
  shouldShowDesktopUpdateButton,
  shouldToastDesktopUpdateActionResult,
} from "./desktopUpdate.logic";

const baseState: DesktopUpdateState = {
  enabled: true,
  status: "idle",
  currentVersion: "1.0.0",
  hostArch: "x64",
  appArch: "x64",
  runningUnderArm64Translation: false,
  availableVersion: null,
  downloadedVersion: null,
  downloadPercent: null,
  checkedAt: null,
  message: null,
  errorContext: null,
  canRetry: false,
};

describe("desktop update button state", () => {
  it("keeps pending backend recovery visible and opens installation recovery", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "error",
      backendRecovery: [
        { environmentId: "primary", label: "Local", reason: "port-in-use", port: 14373 },
      ],
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(true);
    expect(resolveDesktopUpdateButtonAction(state)).toBe("install");
    expect(isDesktopUpdateButtonDisabled(state)).toBe(false);
    expect(getDesktopUpdateButtonTooltip(state)).toBe(
      "Update not installed: Coddy Studio's local server is stopped.",
    );
    expect(canCheckForUpdate(state)).toBe(false);
  });
  it("shows a download action when an update is available", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "available",
      availableVersion: "1.1.0",
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(true);
    expect(resolveDesktopUpdateButtonAction(state)).toBe("download");
  });

  it("keeps retry action available after a download error", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "error",
      availableVersion: "1.1.0",
      message: "network timeout",
      errorContext: "download",
      canRetry: true,
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(true);
    expect(resolveDesktopUpdateButtonAction(state)).toBe("download");
    expect(getDesktopUpdateButtonTooltip(state)).toContain("Click to retry");
  });

  it("keeps install action available after an install error", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "error",
      downloadedVersion: "1.1.0",
      availableVersion: "1.1.0",
      message: "shutdown timeout",
      errorContext: "install",
      canRetry: true,
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(true);
    expect(resolveDesktopUpdateButtonAction(state)).toBe("install");
    expect(getDesktopUpdateButtonTooltip(state)).toContain("Click to retry");
  });

  it("prefers install when a downloaded version already exists", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "available",
      availableVersion: "1.1.0",
      downloadedVersion: "1.1.0",
    };
    expect(resolveDesktopUpdateButtonAction(state)).toBe("install");
  });

  it("hides the button for non-actionable check errors", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "error",
      message: "network unavailable",
      errorContext: "check",
      canRetry: true,
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(false);
    expect(resolveDesktopUpdateButtonAction(state)).toBe("none");
  });

  it("disables the button while downloading", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      status: "downloading",
      availableVersion: "1.1.0",
      downloadPercent: 42.5,
    };
    expect(shouldShowDesktopUpdateButton(state)).toBe(true);
    expect(isDesktopUpdateButtonDisabled(state)).toBe(true);
    expect(getDesktopUpdateButtonTooltip(state)).toContain("42%");
  });

  it("disables duplicate actions while protecting or installing", () => {
    expect(isDesktopUpdateButtonDisabled({ ...baseState, phase: "protecting" })).toBe(true);
    expect(isDesktopUpdateButtonDisabled({ ...baseState, phase: "installing" })).toBe(true);
  });
});

describe("desktop backend recovery copy", () => {
  it("explains a primary port conflict using the actual port", () => {
    expect(
      getDesktopBackendRecoveryMessage({
        environmentId: "primary",
        label: "Windows",
        reason: "port-in-use",
        port: 14373,
      }),
    ).toBe(
      "Coddy Studio's local server couldn't restart: port 14373 is in use by another program. Quit that program, then choose Restart server.",
    );
  });

  it("names a secondary server and explains how to recover another start failure", () => {
    expect(
      getDesktopBackendRecoveryMessage({
        environmentId: "wsl:Ubuntu",
        label: "WSL (Ubuntu)",
        reason: "other",
        port: 14374,
      }),
    ).toBe(
      "Coddy Studio's WSL (Ubuntu) server couldn't restart. Choose Restart server. If that fails, restart Coddy Studio.",
    );
  });
});

describe("getDesktopUpdateActionError", () => {
  it("returns user-visible message for accepted failed attempts", () => {
    const result: DesktopUpdateActionResult = {
      accepted: true,
      completed: false,
      state: {
        ...baseState,
        status: "available",
        availableVersion: "1.1.0",
        message: "checksum mismatch",
        errorContext: "download",
        canRetry: true,
      },
    };
    expect(getDesktopUpdateActionError(result)).toBe("checksum mismatch");
  });

  it("ignores messages for non-accepted attempts", () => {
    const result: DesktopUpdateActionResult = {
      accepted: false,
      completed: false,
      state: {
        ...baseState,
        status: "error",
        message: "background failure",
        errorContext: "check",
        canRetry: false,
      },
    };
    expect(getDesktopUpdateActionError(result)).toBeNull();
  });

  it("ignores messages for successful attempts", () => {
    const result: DesktopUpdateActionResult = {
      accepted: true,
      completed: true,
      state: {
        ...baseState,
        status: "downloaded",
        downloadedVersion: "1.1.0",
        availableVersion: "1.1.0",
        message: null,
        errorContext: null,
        canRetry: true,
      },
    };
    expect(getDesktopUpdateActionError(result)).toBeNull();
  });
});

describe("desktop update UI helpers", () => {
  it("toasts only for actionable updater errors", () => {
    expect(
      shouldToastDesktopUpdateActionResult({
        accepted: true,
        completed: false,
        state: { ...baseState, message: "checksum mismatch" },
      }),
    ).toBe(true);
    expect(
      shouldToastDesktopUpdateActionResult({
        accepted: true,
        completed: false,
        state: { ...baseState, message: null },
      }),
    ).toBe(false);
    expect(
      shouldToastDesktopUpdateActionResult({
        accepted: true,
        completed: true,
        state: { ...baseState, message: "checksum mismatch" },
      }),
    ).toBe(false);
  });

  it("shows an Apple Silicon warning for Intel builds under Rosetta", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      hostArch: "arm64",
      appArch: "x64",
      runningUnderArm64Translation: true,
    };

    expect(shouldShowArm64IntelBuildWarning(state)).toBe(true);
    expect(getArm64IntelBuildWarningDescription(state)).toContain("Apple Silicon");
    expect(getArm64IntelBuildWarningDescription(state)).toContain("Intel build");
  });

  it("changes the warning copy when a native build update is ready to download", () => {
    const state: DesktopUpdateState = {
      ...baseState,
      hostArch: "arm64",
      appArch: "x64",
      runningUnderArm64Translation: true,
      status: "available",
      availableVersion: "1.1.0",
    };

    expect(getArm64IntelBuildWarningDescription(state)).toContain("Download the available update");
  });

  it("includes the downloaded version in the install confirmation copy", () => {
    expect(
      getDesktopUpdateInstallConfirmationMessage({
        availableVersion: "1.1.0",
        downloadedVersion: "1.1.1",
      }),
    ).toContain("Install update 1.1.1 and restart Coddy Studio?");
  });

  it("falls back to generic install confirmation copy when no version is available", () => {
    expect(
      getDesktopUpdateInstallConfirmationMessage({
        availableVersion: null,
        downloadedVersion: null,
      }),
    ).toContain("Install update and restart Coddy Studio?");
  });
});

describe("canCheckForUpdate", () => {
  it("returns false for null state", () => {
    expect(canCheckForUpdate(null)).toBe(false);
  });

  it("returns false when updates are disabled", () => {
    expect(canCheckForUpdate({ ...baseState, enabled: false, status: "disabled" })).toBe(false);
  });

  it("returns false while checking", () => {
    expect(canCheckForUpdate({ ...baseState, status: "checking" })).toBe(false);
  });

  it("returns false while downloading", () => {
    expect(canCheckForUpdate({ ...baseState, status: "downloading", downloadPercent: 50 })).toBe(
      false,
    );
  });

  it("returns false once an update has been downloaded", () => {
    expect(
      canCheckForUpdate({
        ...baseState,
        status: "downloaded",
        availableVersion: "1.1.0",
        downloadedVersion: "1.1.0",
      }),
    ).toBe(false);
  });

  it("returns true when idle", () => {
    expect(canCheckForUpdate({ ...baseState, status: "idle" })).toBe(true);
  });

  it("returns true when up-to-date", () => {
    expect(canCheckForUpdate({ ...baseState, status: "up-to-date" })).toBe(true);
  });

  it("returns true when an update is available", () => {
    expect(
      canCheckForUpdate({ ...baseState, status: "available", availableVersion: "1.1.0" }),
    ).toBe(true);
  });

  it("returns true on error so the user can retry", () => {
    expect(
      canCheckForUpdate({
        ...baseState,
        status: "error",
        errorContext: "check",
        message: "network",
      }),
    ).toBe(true);
  });
});

describe("desktop update defensive branch coverage", () => {
  it("covers non-actionable architecture and button states", () => {
    expect(shouldShowDesktopUpdateButton(null)).toBe(false);
    expect(isDesktopUpdateButtonDisabled(null)).toBe(false);
    expect(shouldShowArm64IntelBuildWarning(null)).toBe(false);
    expect(
      getArm64IntelBuildWarningDescription({
        ...baseState,
        hostArch: "arm64",
        appArch: "arm64",
      }),
    ).toContain("correct architecture");
    expect(
      getArm64IntelBuildWarningDescription({
        ...baseState,
        hostArch: "arm64",
        appArch: "x64",
        status: "downloaded",
        downloadedVersion: "1.1.0",
      }),
    ).toContain("Restart to install");
  });

  it("formats every tooltip fallback", () => {
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "available" })).toContain(
      "Update available ready",
    );
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "downloading" })).toBe(
      "Downloading update",
    );
    expect(
      getDesktopUpdateButtonTooltip({
        ...baseState,
        status: "downloaded",
        availableVersion: "1.1.0",
      }),
    ).toContain("1.1.0 downloaded");
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "downloaded" })).toContain(
      "Update ready downloaded",
    );
    expect(
      getDesktopUpdateButtonTooltip({ ...baseState, status: "error", message: "Offline" }),
    ).toBe("Offline");
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "error" })).toBe("Update failed");
  });

  it("rejects blank action errors and distinguishes highlighted retry contexts", () => {
    expect(
      getDesktopUpdateActionError({
        accepted: true,
        completed: false,
        state: { ...baseState, message: "   " },
      }),
    ).toBeNull();
    expect(shouldHighlightDesktopUpdateError(null)).toBe(false);
    expect(shouldHighlightDesktopUpdateError({ ...baseState, status: "idle" })).toBe(false);
    expect(
      shouldHighlightDesktopUpdateError({
        ...baseState,
        status: "error",
        errorContext: "check",
      }),
    ).toBe(false);
    expect(
      shouldHighlightDesktopUpdateError({
        ...baseState,
        status: "error",
        errorContext: "download",
      }),
    ).toBe(true);
    expect(
      shouldHighlightDesktopUpdateError({
        ...baseState,
        status: "error",
        errorContext: "install",
      }),
    ).toBe(true);
  });

  it("uses an available version when no downloaded version exists", () => {
    expect(
      getDesktopUpdateInstallConfirmationMessage({
        availableVersion: "1.2.0",
        downloadedVersion: null,
      }),
    ).toContain("Install update 1.2.0");
    expect(
      resolveDesktopUpdateButtonAction({
        ...baseState,
        status: "error",
        errorContext: "download",
        availableVersion: null,
      }),
    ).toBe("none");
  });
});

describe("getDesktopUpdateButtonTooltip", () => {
  it("names the stopped secondary server when it is the only recovery entry", () => {
    expect(
      getDesktopUpdateButtonTooltip({
        ...baseState,
        backendRecovery: [
          { environmentId: "wsl:Ubuntu", label: "WSL (Ubuntu)", reason: "other", port: 14374 },
        ],
      }),
    ).toBe("Update not installed: Coddy Studio's WSL (Ubuntu) server is stopped.");
  });

  it("describes multiple stopped servers without implying that clicking restarts them", () => {
    expect(
      getDesktopUpdateButtonTooltip({
        ...baseState,
        backendRecovery: [
          { environmentId: "primary", label: "Local", reason: "port-in-use", port: 14373 },
          { environmentId: "wsl:Ubuntu", label: "WSL (Ubuntu)", reason: "other", port: 14374 },
        ],
      }),
    ).toBe("Update not installed: some of Coddy Studio's servers are stopped.");
  });

  it("returns 'Up to date' for non-actionable states", () => {
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "idle" })).toBe("Up to date");
    expect(getDesktopUpdateButtonTooltip({ ...baseState, status: "up-to-date" })).toBe(
      "Up to date",
    );
  });
});
