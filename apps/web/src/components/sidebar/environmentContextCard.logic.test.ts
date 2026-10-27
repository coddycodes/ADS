import type { ConnectionTarget } from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const compat = vi.hoisted(() => ({ verdict: null as unknown }));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => compat.verdict,
  selectRemoteUpdateControlCapability: (serverConfig: unknown) => serverConfig !== null,
}));

import {
  buildEnvironmentContextCardView,
  resolveCompatBadge,
} from "./environmentContextCard.logic";

const remoteTarget = {
  _tag: "BearerConnectionTarget",
  connectionId: "paired-1",
} as ConnectionTarget;

function view(overrides: Partial<Parameters<typeof buildEnvironmentContextCardView>[0]> = {}) {
  return buildEnvironmentContextCardView({
    label: "AI-SERVER",
    target: remoteTarget,
    connection: { phase: "connected", error: null, traceId: null },
    serverConfig: {
      environment: { serverVersion: "0.4.2", capabilities: {} },
    } as unknown as ServerConfig,
    ...overrides,
  });
}

describe("buildEnvironmentContextCardView", () => {
  it("uses operator steps for a manually installed server", () => {
    expect(
      view({
        serverConfig: {
          environment: {
            serverVersion: "0.7.2",
            remoteUpdateSupport: { installMode: "manual" },
            capabilities: {},
          },
        } as unknown as ServerConfig,
      })?.manualUpdates,
    ).toBe(true);
    expect(
      view({
        serverConfig: {
          environment: {
            serverVersion: "0.7.2",
            remoteUpdateSupport: { installMode: "interactive" },
            capabilities: {},
          },
        } as unknown as ServerConfig,
      })?.manualUpdates,
    ).toBe(false);
  });
  it("is hidden for primary and desktop-local targets", () => {
    expect(view({ target: { _tag: "PrimaryConnectionTarget" } as ConnectionTarget })).toBeNull();
    expect(
      view({
        target: {
          _tag: "BearerConnectionTarget",
          connectionId: "local:wsl-ubuntu",
        } as ConnectionTarget,
      }),
    ).toBeNull();
  });

  it("renders name, status text, and the Coddy Studio version line for a remote", () => {
    compat.verdict = { kind: "compatible" };
    const card = view();
    expect(card?.name).toBe("AI-SERVER");
    expect(card?.statusText).toBe("Connected");
    expect(card?.versionLine).toBe("Coddy Studio v0.4.2");
    expect(card?.compatBadge).toBeNull();
    expect(card?.showUpdateActions).toBe(true);
  });

  it("degrades without a delivered server config", () => {
    compat.verdict = null;
    const card = view({
      serverConfig: null,
      connection: { phase: "reconnecting", error: "boom", traceId: null },
    });
    expect(card?.versionLine).toBeNull();
    expect(card?.compatBadge).toBeNull();
    expect(card?.showUpdateActions).toBe(false);
    expect(card?.statusText).toContain("Reconnecting");
  });

  it.each([
    { phase: "reconnecting", error: "No data for 30 seconds.", hasReason: true },
    { phase: "error", error: "The pairing credential expired.", hasReason: true },
    { phase: "connected", error: "A stale failure from before.", hasReason: false },
    { phase: "reconnecting", error: "", hasReason: false },
    { phase: "reconnecting", error: null, hasReason: false },
  ] as const)(
    "shows a reason only when the status text carries one ($phase, error $error)",
    ({ phase, error, hasReason }) => {
      const card = view({ connection: { phase, error, traceId: null } });
      expect(card?.hasReason).toBe(hasReason);
      if (hasReason) expect(card?.statusText).toContain(error);
    },
  );
});

describe("resolveCompatBadge", () => {
  it("maps verdicts to badge copy", () => {
    expect(resolveCompatBadge(null)).toBeNull();
    expect(resolveCompatBadge({ kind: "compatible" })).toBeNull();
    expect(resolveCompatBadge({ kind: "legacy" })).toEqual({
      label: "Limited compatibility",
      tone: "warning",
    });
    expect(
      resolveCompatBadge({ kind: "server-too-old", serverVersion: 0, minSupported: 1 }),
    ).toEqual({ label: "Server update required", tone: "error" });
    expect(
      resolveCompatBadge({ kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 }),
    ).toEqual({ label: "App update required", tone: "error" });
  });
});
