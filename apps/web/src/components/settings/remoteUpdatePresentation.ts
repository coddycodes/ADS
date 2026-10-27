import type { RemoteUpdateActiveWork, RemoteUpdateSnapshot } from "@coddy/contracts";
import {
  isRemoteUpdateRunActive,
  type RemoteUpdateFailure,
  type RemoteUpdateRunState,
} from "@coddy/client-runtime/state/remoteUpdateCoordinator";
import { compareSemverVersions } from "@coddy/shared/semver";

export interface RemoteUpdateConfirmationInput {
  readonly name: string;
  readonly targetVersion: string | null;
  /** Null when the server cannot count or the read failed. */
  readonly activeWork: RemoteUpdateActiveWork | null;
  readonly counting: boolean;
  readonly appVersion: string;
}

export interface RemoteUpdateConfirmation {
  readonly title: string;
  readonly lines: ReadonlyArray<string>;
  readonly confirmLabel: string;
  readonly cancelLabel: string;
}

const KEPT =
  "Conversations and queued messages are kept, and agents continue when you send the next message.";

export interface ManualUpdateStepsInput {
  readonly installKind: string;
  readonly os: "darwin" | "linux" | "windows" | "unknown";
  readonly arch: "arm64" | "x64" | "other";
  readonly sshLaunched: boolean;
  readonly serverVersion: string;
}

/** Operator steps use the host's distribution and shell, never the client's platform. */
export function manualUpdateSteps(input: ManualUpdateStepsInput): string {
  const running = `# Currently running: v${input.serverVersion}`;
  const generic = [
    "# Update this Coddy Studio server on its host:",
    "# 1. Stop the running server (Ctrl+C or its service manager).",
    "# 2. Install the new coddy-studio distribution for this host.",
    "# 3. Restart it or its service:",
    "coddy-studio serve",
    "",
    running,
  ];
  if (input.sshLaunched) {
    return [
      "# This server was started by Coddy Studio over SSH.",
      "# 1. Install the new coddy-studio on the host; check coddy-studio --version.",
      "# 2. Locate this connection's pid record under ~/.coddy/studio/ssh-launch/.",
      "# Verify that it still belongs to this coddy-studio server before stopping it.",
      "# 3. Reconnect from Coddy Studio to start the new server.",
      "",
      running,
    ].join("\n");
  }
  if (input.os === "unknown" || input.arch === "other") return generic.join("\n");
  const archiveArch = input.arch === "arm64" ? "aarch64" : "x86_64";
  const release =
    "# Replace VERSION with the release to install from https://github.com/mubeda/CoddyStudio/releases.";
  if (input.installKind === "archive") {
    const asset = `coddy-studio-server-vVERSION-${input.os}-${archiveArch}.${input.os === "windows" ? "zip" : "tar.gz"}`;
    const check =
      input.os === "windows"
        ? [
            `Get-FileHash -Algorithm SHA256 '${asset}'`,
            `# Compare its Hash with the ${asset} line in coddy-studio-server-SHA256SUMS.`,
          ]
        : [
            `grep "  ${asset}$" coddy-studio-server-SHA256SUMS | ${input.os === "darwin" ? "shasum -a 256 -c" : "sha256sum --check -"}`,
          ];
    return [
      release,
      `# 1. Download ${asset} and coddy-studio-server-SHA256SUMS.`,
      "# 2. Check the download:",
      ...check,
      "# 3. Stop the server, replace the extracted distribution, and restart it or its service:",
      "coddy-studio serve",
      "",
      running,
    ].join("\n");
  }
  if (input.installKind === "system-package" && input.os === "linux") {
    return [
      release,
      "# 1. Download this host's package and check it against coddy-studio-server-SHA256SUMS.",
      "# 2. Use the command for your distribution:",
      `sudo apt install ./coddy-studio-server_VERSION_${input.arch === "arm64" ? "arm64" : "amd64"}.deb`,
      `sudo dnf install ./coddy-studio-server-VERSION-1.${archiveArch}.rpm`,
      "# 3. Restart the server or its service.",
      "",
      running,
    ].join("\n");
  }
  return generic.join("\n");
}
const PROTECTION_STAGES = new Set([
  "waiting-for-mutations",
  "quiescing-runtime",
  "acquiring-store-lock",
  "checkpointing-database",
  "creating-verified-backup",
]);

export function remoteUpdateActionLabel(latestVersion: string): string {
  return `Update to v${latestVersion}…`;
}

function workLine(activeWork: RemoteUpdateActiveWork | null): string {
  if (activeWork === null) return `Running agents and terminals on it will stop. ${KEPT}`;
  const parts: string[] = [];
  if (activeWork.runningTurns > 0) {
    parts.push(
      `${activeWork.runningTurns} running agent${activeWork.runningTurns === 1 ? "" : "s"}`,
    );
  }
  if (activeWork.liveTerminals > 0) {
    parts.push(`${activeWork.liveTerminals} terminal${activeWork.liveTerminals === 1 ? "" : "s"}`);
  }
  return parts.length === 0
    ? "Nothing is running on it now."
    : `${parts.join(" and ")} will stop. ${KEPT}`;
}

export function remoteUpdateConfirmation(
  input: RemoteUpdateConfirmationInput,
): RemoteUpdateConfirmation {
  const lines = [
    `Updating ${input.name} restarts Coddy Studio there.`,
    input.counting ? "Counting running work…" : workLine(input.activeWork),
  ];
  if (
    input.targetVersion !== null &&
    compareSemverVersions(input.targetVersion, input.appVersion) > 0
  ) {
    lines.push(
      `v${input.targetVersion} is newer than this app (v${input.appVersion}). Update this app too.`,
    );
  }
  return {
    title:
      input.targetVersion === null
        ? `Update ${input.name}?`
        : `Update ${input.name} to v${input.targetVersion}?`,
    lines,
    confirmLabel: `Update ${input.name}`,
    cancelLabel: "Cancel",
  };
}

export function remoteUpdateInstallStageLabel(stage: string | null): string {
  if (stage !== null && PROTECTION_STAGES.has(stage)) return "Backing up project data…";
  if (stage === "stopping-backend" || stage === "installing") return "Restarting…";
  return "Updating…";
}

export function remoteUpdateProgressLabel(run: RemoteUpdateRunState): string | null {
  switch (run.phase) {
    case "queued":
      return "Queued";
    case "starting":
      return "Updating…";
    case "downloading":
      return run.percent === null ? "Downloading…" : `Downloading ${Math.floor(run.percent)}%`;
    case "installing":
      return remoteUpdateInstallStageLabel(run.stage);
    case "restarting":
      return "Restarting…";
    case "verifying":
      return "Checking the new version…";
    case "succeeded":
      return `Updated to v${run.version}`;
    case "up-to-date":
      return "Already up to date";
    case "failed":
      return null;
  }
}

function sentence(text: string): string {
  return /[.!?]$/u.test(text) ? text : `${text}.`;
}

export function remoteUpdateFailureMessage(name: string, failure: RemoteUpdateFailure): string {
  switch (failure.kind) {
    case "host-error":
      return failure.runningVersion === "unknown"
        ? `Couldn't update ${name}: ${sentence(failure.message)} Nothing was installed.`
        : `Couldn't update ${name}: ${sentence(failure.message)} Nothing was installed; it runs v${failure.runningVersion} again.`;
    case "not-back":
      return `${name} hasn't come back after the update. Check Coddy Studio on ${name}; it may be on a different port.`;
    case "wrong-version":
      return `${name} restarted on v${failure.runningVersion} instead of v${failure.targetVersion}.`;
    case "manual-required":
      return `Couldn't update ${name}: this server must be updated manually.`;
    case "install-timeout":
      return `${name} is taking longer than 2 minutes to install ${failure.targetVersion === null ? "the update" : `v${failure.targetVersion}`}. Check Coddy Studio on ${name}.`;
    case "blocked":
      return `Can't reconnect to ${name} after the update: ${sentence(failure.message)}`;
  }
}

export function remoteUpdateSuccessTitle(name: string, version: string): string {
  return `${name} updated to v${version}`;
}

export function remoteUpdateUpToDateTitle(name: string): string {
  return `${name} is already up to date`;
}

/** Hide settled feedback only when the snapshot catches up; keep the coordinator's state intact. */
export function visibleRemoteUpdateRun(
  run: RemoteUpdateRunState | null,
  snapshot: RemoteUpdateSnapshot | null | undefined,
): RemoteUpdateRunState | null {
  if (run === null || isRemoteUpdateRunActive(run) || run.phase === "failed") return run;
  if (
    run.phase === "succeeded" &&
    snapshot != null &&
    compareSemverVersions(snapshot.serverVersion, run.version) >= 0
  ) {
    return null;
  }
  if (run.phase === "up-to-date" && snapshot?.state === "up-to-date") return null;
  return run;
}
