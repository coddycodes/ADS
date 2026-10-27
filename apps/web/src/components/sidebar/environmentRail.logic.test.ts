import type { ConnectionTarget } from "@coddy/client-runtime/connection";
import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildEnvironmentRailModel,
  environmentLetterAvatar,
  resolveAddProjectTargetLabel,
  resolveEnvironmentRailStatus,
  selectRailVisibleEnvironmentIds,
  toEnvironmentRailCandidate,
  type EnvironmentRailCandidate,
} from "./environmentRail.logic";

const ENV_PRIMARY = EnvironmentId.make("env-primary");
const ENV_WSL = EnvironmentId.make("env-wsl");
const ENV_REMOTE_A = EnvironmentId.make("env-remote-a");
const ENV_REMOTE_B = EnvironmentId.make("env-remote-b");

function candidate(
  overrides: Partial<EnvironmentRailCandidate> & { environmentId: EnvironmentId },
): EnvironmentRailCandidate {
  return {
    label: "AI-SERVER",
    isPrimary: false,
    isDesktopLocal: false,
    phase: "connected",
    compat: null,
    updateAvailable: false,
    ...overrides,
  };
}

const primary = candidate({ environmentId: ENV_PRIMARY, label: "Local", isPrimary: true });
const wsl = candidate({ environmentId: ENV_WSL, label: "Ubuntu", isDesktopLocal: true });
const remoteA = candidate({ environmentId: ENV_REMOTE_A, label: "AI-SERVER" });
const remoteB = candidate({ environmentId: ENV_REMOTE_B, label: "build-farm", phase: "error" });

describe("toEnvironmentRailCandidate", () => {
  const asCandidate = (target: ConnectionTarget, updateAvailable = false) =>
    toEnvironmentRailCandidate({
      environmentId: ENV_REMOTE_A,
      label: "x",
      target,
      phase: "connected",
      compat: null,
      updateAvailable,
    });

  it("classifies primary, desktop-local, and remote targets", () => {
    expect(asCandidate({ _tag: "PrimaryConnectionTarget" } as ConnectionTarget).isPrimary).toBe(
      true,
    );
    const local = asCandidate({
      _tag: "BearerConnectionTarget",
      connectionId: "local:wsl-ubuntu",
    } as ConnectionTarget);
    expect(local.isDesktopLocal).toBe(true);
    const remote = asCandidate({
      _tag: "BearerConnectionTarget",
      connectionId: "paired-1",
    } as ConnectionTarget);
    expect(remote.isPrimary).toBe(false);
    expect(remote.isDesktopLocal).toBe(false);
  });

  it("passes the updateAvailable flag through", () => {
    const target = {
      _tag: "BearerConnectionTarget",
      connectionId: "paired-1",
    } as ConnectionTarget;
    expect(asCandidate(target, false).updateAvailable).toBe(false);
    expect(asCandidate(target, true).updateAvailable).toBe(true);
  });
});

describe("resolveEnvironmentRailStatus", () => {
  it("maps phases and verdicts to the four status-dot states", () => {
    const status = (input: Partial<Parameters<typeof resolveEnvironmentRailStatus>[0]>) =>
      resolveEnvironmentRailStatus({
        phase: "connected",
        compat: null,
        updateAvailable: false,
        ...input,
      });
    expect(status({})).toBe("connected");
    expect(status({ compat: { kind: "compatible" } })).toBe("connected");
    expect(status({ phase: "available" })).toBe("disconnected");
    expect(status({ phase: "offline" })).toBe("disconnected");
    expect(status({ phase: "reconnecting" })).toBe("disconnected");
    expect(status({ phase: "error" })).toBe("error");
    expect(status({ compat: { kind: "legacy" } })).toBe("attention");
    expect(status({ updateAvailable: true })).toBe("attention");
    expect(status({ compat: { kind: "server-too-old", serverVersion: 0, minSupported: 1 } })).toBe(
      "error",
    );
    expect(
      status({ compat: { kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 } }),
    ).toBe("error");
    expect(status({ phase: "available", compat: { kind: "legacy" } })).toBe("disconnected");
  });
});

describe("environmentLetterAvatar", () => {
  it("derives one- and two-word initials", () => {
    expect(environmentLetterAvatar("AI-SERVER")).toBe("AS");
    expect(environmentLetterAvatar("build farm")).toBe("BF");
    expect(environmentLetterAvatar("staging")).toBe("ST");
    expect(environmentLetterAvatar("x")).toBe("X");
    expect(environmentLetterAvatar("  ")).toBe("?");
  });
});

describe("buildEnvironmentRailModel", () => {
  it("groups locals under one entry and sorts remotes by label", () => {
    const model = buildEnvironmentRailModel({
      candidates: [remoteB, primary, wsl, remoteA],
      activeEnvironmentId: ENV_PRIMARY,
    });
    expect(model.localSelected).toBe(true);
    expect(model.localTargetEnvironmentId).toBe(ENV_PRIMARY);
    expect(model.localSubEntries.map((entry) => entry.environmentId)).toEqual([
      ENV_PRIMARY,
      ENV_WSL,
    ]);
    expect(model.localSubEntries[0]?.label).toBe("This device");
    expect(model.remotes.map((entry) => entry.label)).toEqual(["AI-SERVER", "build-farm"]);
    expect(model.remotes[1]?.status).toBe("error");
  });

  it("has no sub-picker without desktop-local backends and treats null active as Local", () => {
    const model = buildEnvironmentRailModel({
      candidates: [primary, remoteA],
      activeEnvironmentId: null,
    });
    expect(model.localSubEntries).toEqual([]);
    expect(model.localSelected).toBe(true);
    expect(model.remotes[0]?.selected).toBe(false);
  });

  it("marks the active remote selected and Local unselected", () => {
    const model = buildEnvironmentRailModel({
      candidates: [primary, remoteA],
      activeEnvironmentId: ENV_REMOTE_A,
    });
    expect(model.localSelected).toBe(false);
    expect(model.remotes[0]?.selected).toBe(true);
  });
});

describe("selectRailVisibleEnvironmentIds", () => {
  const scope = [
    { environmentId: ENV_PRIMARY, isLocal: true },
    { environmentId: ENV_WSL, isLocal: true },
    { environmentId: ENV_REMOTE_A, isLocal: false },
  ];

  it("filters a null selection to local environments", () => {
    expect(
      selectRailVisibleEnvironmentIds({ candidates: scope, activeEnvironmentId: null }),
    ).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("scopes an unresolvable selection to Local", () => {
    expect(
      selectRailVisibleEnvironmentIds({
        candidates: scope,
        activeEnvironmentId: ENV_REMOTE_B,
      }),
    ).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("applies no filter for a degenerate catalog without a local environment", () => {
    const remoteOnly = [{ environmentId: ENV_REMOTE_A, isLocal: false }];
    expect(
      selectRailVisibleEnvironmentIds({ candidates: remoteOnly, activeEnvironmentId: null }),
    ).toBeNull();
    expect(
      selectRailVisibleEnvironmentIds({
        candidates: remoteOnly,
        activeEnvironmentId: ENV_REMOTE_A,
      }),
    ).toEqual(new Set([ENV_REMOTE_A]));
  });

  it("shows the union of local environments when a local one is active", () => {
    expect(
      selectRailVisibleEnvironmentIds({ candidates: scope, activeEnvironmentId: ENV_WSL }),
    ).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("shows only the selected remote environment", () => {
    expect(
      selectRailVisibleEnvironmentIds({
        candidates: scope,
        activeEnvironmentId: ENV_REMOTE_A,
      }),
    ).toEqual(new Set([ENV_REMOTE_A]));
  });
});

describe("resolveAddProjectTargetLabel", () => {
  const labeled = [
    { environmentId: ENV_PRIMARY, isLocal: true, label: "Local" },
    { environmentId: ENV_REMOTE_A, isLocal: false, label: "AI-SERVER" },
  ];

  it("names the remote target and stays silent for Local or unknown", () => {
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: ENV_REMOTE_A }),
    ).toBe("AI-SERVER");
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: ENV_PRIMARY }),
    ).toBeNull();
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: null }),
    ).toBeNull();
  });
});
