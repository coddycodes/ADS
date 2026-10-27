import type { RemoteUpdateActiveWork, RemoteUpdateSnapshot } from "@coddy/contracts";
import type {
  RemoteUpdateFailure,
  RemoteUpdateRunState,
} from "@coddy/client-runtime/state/remoteUpdateCoordinator";
import { describe, expect, it } from "vite-plus/test";

import {
  remoteUpdateActionLabel,
  remoteUpdateConfirmation,
  remoteUpdateFailureMessage,
  remoteUpdateInstallStageLabel,
  remoteUpdateProgressLabel,
  remoteUpdateSuccessTitle,
  remoteUpdateUpToDateTitle,
  visibleRemoteUpdateRun,
  manualUpdateSteps,
} from "./remoteUpdatePresentation";

describe("manualUpdateSteps", () => {
  const base = { serverVersion: "0.7.2", sshLaunched: false } as const;
  it("names the archive and checksum command for the host OS and architecture", () => {
    const linux = manualUpdateSteps({
      ...base,
      installKind: "archive",
      os: "linux",
      arch: "arm64",
    });
    expect(linux).toContain("coddy-studio-server-vVERSION-linux-aarch64.tar.gz");
    expect(linux).toContain("coddy-studio-server-SHA256SUMS");
    expect(linux).toContain("sha256sum --check -");
    expect(
      manualUpdateSteps({ ...base, installKind: "archive", os: "darwin", arch: "x64" }),
    ).toContain("shasum -a 256 -c");
    const windows = manualUpdateSteps({
      ...base,
      installKind: "archive",
      os: "windows",
      arch: "x64",
    });
    expect(windows).toContain("coddy-studio-server-vVERSION-windows-x86_64.zip");
    expect(windows).toContain("Get-FileHash");
    expect(windows).not.toContain("grep");
  });
  it("gives platform package options and preserves running version context", () => {
    const steps = manualUpdateSteps({
      ...base,
      installKind: "system-package",
      os: "linux",
      arch: "x64",
    });
    expect(steps).toContain("sudo apt install ./coddy-studio-server_VERSION_amd64.deb");
    expect(steps).toContain("sudo dnf install ./coddy-studio-server-VERSION-1.x86_64.rpm");
    expect(steps).toContain("Currently running: v0.7.2");
  });
  it("instructs an SSH operator to verify this server and reconnect without stopping every recorded pid", () => {
    const steps = manualUpdateSteps({
      ...base,
      installKind: "unknown",
      os: "linux",
      arch: "x64",
      sshLaunched: true,
    });
    expect(steps).toContain("~/.coddy/studio/ssh-launch/");
    expect(steps).toContain("Reconnect");
    expect(steps).toContain("Verify");
    expect(steps).not.toContain("kill");
  });
  it("uses generic operator guidance when platform or install kind is unknown", () => {
    const steps = manualUpdateSteps({
      ...base,
      installKind: "flatpak",
      os: "unknown",
      arch: "other",
    });
    expect(steps).toContain("coddy-studio serve");
    expect(steps).not.toContain("linux-");
  });
});

const base = {
  name: "Ai-server",
  targetVersion: "0.6.4",
  appVersion: "0.6.4",
  counting: false,
};
const KEPT =
  "Conversations and queued messages are kept, and agents continue when you send the next message.";
const snapshot: RemoteUpdateSnapshot = {
  serverVersion: "0.6.2",
  latestVersion: "0.6.4",
  state: "update-available",
  error: null,
  support: { installMode: "interactive", reason: "available", installKind: "unknown" },
  downloadPercent: null,
  targetVersion: null,
  installStage: null,
};

describe("remoteUpdateConfirmation", () => {
  it("names the server, target and both confirmation actions", () => {
    expect(
      remoteUpdateConfirmation({
        ...base,
        activeWork: { runningTurns: 2, liveTerminals: 3, queuedMessages: 1 },
      }),
    ).toEqual({
      title: "Update Ai-server to v0.6.4?",
      lines: [
        "Updating Ai-server restarts Coddy Studio there.",
        `2 running agents and 3 terminals will stop. ${KEPT}`,
      ],
      confirmLabel: "Update Ai-server",
      cancelLabel: "Cancel",
    });
  });

  it("omits the target when it is unknown", () => {
    const confirmation = remoteUpdateConfirmation({
      ...base,
      targetVersion: null,
      activeWork: null,
    });
    expect(confirmation.title).toBe("Update Ai-server?");
    expect(confirmation.lines).toEqual([
      "Updating Ai-server restarts Coddy Studio there.",
      `Running agents and terminals on it will stop. ${KEPT}`,
    ]);
  });

  it.each<[RemoteUpdateActiveWork | null, string]>([
    [
      { runningTurns: 1, liveTerminals: 1, queuedMessages: 0 },
      `1 running agent and 1 terminal will stop. ${KEPT}`,
    ],
    [
      { runningTurns: 1, liveTerminals: 0, queuedMessages: 0 },
      `1 running agent will stop. ${KEPT}`,
    ],
    [{ runningTurns: 0, liveTerminals: 1, queuedMessages: 0 }, `1 terminal will stop. ${KEPT}`],
    [
      { runningTurns: 2, liveTerminals: 0, queuedMessages: 0 },
      `2 running agents will stop. ${KEPT}`,
    ],
    [{ runningTurns: 0, liveTerminals: 3, queuedMessages: 0 }, `3 terminals will stop. ${KEPT}`],
    [{ runningTurns: 0, liveTerminals: 0, queuedMessages: 4 }, "Nothing is running on it now."],
    [null, `Running agents and terminals on it will stop. ${KEPT}`],
  ])("describes running work %j", (activeWork, expected) => {
    expect(remoteUpdateConfirmation({ ...base, activeWork }).lines[1]).toBe(expected);
  });

  it.each<RemoteUpdateActiveWork | null>([
    null,
    { runningTurns: 2, liveTerminals: 3, queuedMessages: 1 },
  ])("shows counting instead of unavailable or old counts %j", (activeWork) => {
    expect(remoteUpdateConfirmation({ ...base, activeWork, counting: true }).lines[1]).toBe(
      "Counting running work…",
    );
  });

  it.each([
    ["0.6.4", "0.6.3"],
    ["0.10.0", "0.9.9"],
    ["0.6.4", "0.6.4-beta.1"],
  ])("warns when target %s is newer than app %s by semver", (targetVersion, appVersion) => {
    expect(
      remoteUpdateConfirmation({ ...base, targetVersion, appVersion, activeWork: null }).lines,
    ).toEqual([
      "Updating Ai-server restarts Coddy Studio there.",
      `Running agents and terminals on it will stop. ${KEPT}`,
      `v${targetVersion} is newer than this app (v${appVersion}). Update this app too.`,
    ]);
  });

  it.each<[string | null, string]>([
    ["0.6.4", "0.6.4"],
    ["0.6.4", "0.6.5"],
    ["0.9.9", "0.10.0"],
    ["0.6.4-beta.1", "0.6.4"],
    [null, "0.6.4"],
  ])("omits the newer-app warning for target %s and app %s", (targetVersion, appVersion) => {
    expect(
      remoteUpdateConfirmation({ ...base, targetVersion, appVersion, activeWork: null }).lines,
    ).toHaveLength(2);
  });
});

describe("remote update progress and outcome", () => {
  it("names the update action and successful outcomes", () => {
    expect(remoteUpdateActionLabel("0.6.4")).toBe("Update to v0.6.4…");
    expect(remoteUpdateSuccessTitle("Ai-server", "0.6.4")).toBe("Ai-server updated to v0.6.4");
    expect(remoteUpdateUpToDateTitle("Ai-server")).toBe("Ai-server is already up to date");
  });

  it.each<[RemoteUpdateRunState, string | null]>([
    [{ phase: "queued" }, "Queued"],
    [{ phase: "starting" }, "Updating…"],
    [{ phase: "downloading", percent: 42.9, targetVersion: "0.6.4" }, "Downloading 42%"],
    [{ phase: "downloading", percent: 0, targetVersion: null }, "Downloading 0%"],
    [{ phase: "downloading", percent: null, targetVersion: null }, "Downloading…"],
    [
      { phase: "installing", stage: "creating-verified-backup", targetVersion: "0.6.4" },
      "Backing up project data…",
    ],
    [{ phase: "installing", stage: "installing", targetVersion: "0.6.4" }, "Restarting…"],
    [{ phase: "installing", stage: null, targetVersion: null }, "Updating…"],
    [{ phase: "restarting", targetVersion: "0.6.4" }, "Restarting…"],
    [{ phase: "verifying", targetVersion: "0.6.4" }, "Checking the new version…"],
    [{ phase: "succeeded", version: "0.6.4" }, "Updated to v0.6.4"],
    [{ phase: "up-to-date" }, "Already up to date"],
    [{ phase: "failed", failure: { kind: "not-back" } }, null],
  ])("labels run %j", (run, expected) => {
    expect(remoteUpdateProgressLabel(run)).toBe(expected);
  });

  it.each([
    ["waiting-for-mutations", "Backing up project data…"],
    ["quiescing-runtime", "Backing up project data…"],
    ["acquiring-store-lock", "Backing up project data…"],
    ["checkpointing-database", "Backing up project data…"],
    ["creating-verified-backup", "Backing up project data…"],
    ["stopping-backend", "Restarting…"],
    ["installing", "Restarting…"],
    ["defragmenting-disk", "Updating…"],
    [null, "Updating…"],
  ])("labels install stage %s", (stage, expected) => {
    expect(remoteUpdateInstallStageLabel(stage)).toBe(expected);
  });
});

describe("remoteUpdateFailureMessage", () => {
  it.each<[RemoteUpdateFailure, string]>([
    [
      { kind: "host-error", message: "Signature mismatch", runningVersion: "0.6.2" },
      "Couldn't update Ai-server: Signature mismatch. Nothing was installed; it runs v0.6.2 again.",
    ],
    [
      { kind: "host-error", message: "The server did not answer.", runningVersion: "unknown" },
      "Couldn't update Ai-server: The server did not answer. Nothing was installed.",
    ],
    [
      { kind: "not-back" },
      "Ai-server hasn't come back after the update. Check Coddy Studio on Ai-server; it may be on a different port.",
    ],
    [
      { kind: "wrong-version", runningVersion: "0.6.2", targetVersion: "0.6.4" },
      "Ai-server restarted on v0.6.2 instead of v0.6.4.",
    ],
    [
      { kind: "manual-required" },
      "Couldn't update Ai-server: this server must be updated manually.",
    ],
    [
      { kind: "install-timeout", targetVersion: "0.6.4" },
      "Ai-server is taking longer than 2 minutes to install v0.6.4. Check Coddy Studio on Ai-server.",
    ],
    [
      { kind: "install-timeout", targetVersion: null },
      "Ai-server is taking longer than 2 minutes to install the update. Check Coddy Studio on Ai-server.",
    ],
    [
      { kind: "blocked", message: "Pair this server again" },
      "Can't reconnect to Ai-server after the update: Pair this server again.",
    ],
  ])("explains failure %j", (failure, expected) => {
    expect(remoteUpdateFailureMessage("Ai-server", failure)).toBe(expected);
  });

  it.each([".", "!", "?"])("preserves a host message's final %s", (punctuation) => {
    const message = `Check the host${punctuation}`;
    for (const runningVersion of ["unknown", "0.6.2"]) {
      expect(
        remoteUpdateFailureMessage("Ai-server", { kind: "host-error", message, runningVersion }),
      ).toBe(
        `Couldn't update Ai-server: ${message} Nothing was installed${runningVersion === "unknown" ? "." : "; it runs v0.6.2 again."}`,
      );
    }
    expect(remoteUpdateFailureMessage("Ai-server", { kind: "blocked", message })).toBe(
      `Can't reconnect to Ai-server after the update: ${message}`,
    );
  });
});

describe("visibleRemoteUpdateRun", () => {
  it("keeps no run as null", () => {
    expect(visibleRemoteUpdateRun(null, snapshot)).toBeNull();
    expect(visibleRemoteUpdateRun(null, null)).toBeNull();
  });

  it.each<RemoteUpdateRunState>([
    { phase: "queued" },
    { phase: "starting" },
    { phase: "downloading", percent: 42, targetVersion: "0.6.4" },
    { phase: "installing", stage: "installing", targetVersion: "0.6.4" },
    { phase: "restarting", targetVersion: "0.6.4" },
    { phase: "verifying", targetVersion: "0.6.4" },
    { phase: "failed", failure: { kind: "not-back" } },
  ])("retains %j even when a snapshot reports the target is up to date", (run) => {
    expect(visibleRemoteUpdateRun(run, null)).toBe(run);
    expect(
      visibleRemoteUpdateRun(run, { ...snapshot, state: "up-to-date", serverVersion: "0.6.4" }),
    ).toBe(run);
  });

  it("retains success until a snapshot catches up, without mutating the run", () => {
    const run: RemoteUpdateRunState = Object.freeze({ phase: "succeeded", version: "0.6.4" });
    expect(visibleRemoteUpdateRun(run, null)).toBe(run);
    expect(visibleRemoteUpdateRun(run, undefined)).toBe(run);
    expect(visibleRemoteUpdateRun(run, { ...snapshot, state: "up-to-date" })).toBe(run);
    expect(visibleRemoteUpdateRun(run, { ...snapshot, serverVersion: "0.6.4-beta.1" })).toBe(run);
    expect(visibleRemoteUpdateRun(run, { ...snapshot, serverVersion: "0.6.4" })).toBeNull();
    expect(visibleRemoteUpdateRun(run, { ...snapshot, serverVersion: "0.10.0" })).toBeNull();
    expect(visibleRemoteUpdateRun(run, snapshot)).toBe(run);
  });

  it("retains up-to-date until the snapshot explicitly agrees", () => {
    const run: RemoteUpdateRunState = Object.freeze({ phase: "up-to-date" });
    expect(visibleRemoteUpdateRun(run, null)).toBe(run);
    expect(visibleRemoteUpdateRun(run, snapshot)).toBe(run);
    expect(visibleRemoteUpdateRun(run, { ...snapshot, serverVersion: "0.10.0" })).toBe(run);
    expect(visibleRemoteUpdateRun(run, { ...snapshot, state: "up-to-date" })).toBeNull();
    expect(visibleRemoteUpdateRun(run, snapshot)).toBe(run);
  });
});
