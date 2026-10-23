import type { ServerConfig } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Option from "effect/Option";
import { Atom, AtomRegistry } from "effect/reactivity";

import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionBlockedError,
  ConnectionStorageChangedError,
  PrimaryConnectionTarget,
  type SupervisorConnectionState,
} from "../connection/model.ts";
import type { EnvironmentShellState } from "./shell.ts";
import type { ConnectionCatalogHealth } from "../platform/persistence.ts";
import {
  createEnvironmentServerConfigsAtom,
  createEnvironmentShellSummaryAtom,
  resolveEnvironmentAvailabilityStatus,
} from "./shell.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const OTHER_ENVIRONMENT_ID = EnvironmentId.make("environment-2");

function environmentEntry(environmentId: EnvironmentId, label: string) {
  return {
    target: new PrimaryConnectionTarget({
      environmentId,
      label,
      httpBaseUrl: `https://${environmentId}.example.test`,
      wsBaseUrl: `wss://${environmentId}.example.test`,
    }),
    profile: Option.none(),
  };
}

function shellState(input: {
  readonly status: EnvironmentShellState["status"];
  readonly updatedAt?: string;
  readonly error?: string;
  readonly snapshotSequence?: number;
}): EnvironmentShellState {
  return {
    snapshot:
      input.updatedAt === undefined
        ? Option.none()
        : Option.some({
            snapshotSequence: input.snapshotSequence ?? 1,
            updatedAt: input.updatedAt,
            projects: [],
            threads: [],
          }),
    status: input.status,
    error: input.error === undefined ? Option.none() : Option.some(input.error),
  };
}

function makeHarness() {
  const shellStateAtoms = Atom.family((environmentId: EnvironmentId) =>
    Atom.make<EnvironmentShellState>(
      environmentId === ENVIRONMENT_ID
        ? shellState({
            status: "degraded",
            updatedAt: "2026-06-01T00:00:00.000Z",
          })
        : shellState({
            status: "synchronizing",
            updatedAt: "2026-06-02T00:00:00.000Z",
            error: "Retrying.",
          }),
    ),
  );
  const configAtoms = Atom.family((_environmentId: EnvironmentId) =>
    Atom.make<ServerConfig | null>(null),
  );
  const catalogValueAtom = Atom.make({
    isReady: true,
    entries: new Map([
      [ENVIRONMENT_ID, environmentEntry(ENVIRONMENT_ID, "Environment")],
      [OTHER_ENVIRONMENT_ID, environmentEntry(OTHER_ENVIRONMENT_ID, "Other environment")],
    ]),
  });
  const summaryAtom = createEnvironmentShellSummaryAtom({
    catalogValueAtom,
    shellStateValueAtom: shellStateAtoms,
  });
  const serverConfigsAtom = createEnvironmentServerConfigsAtom({
    catalogValueAtom,
    serverConfigValueAtom: configAtoms,
  });

  return {
    registry: AtomRegistry.make(),
    shellStateAtom: shellStateAtoms,
    configAtom: configAtoms,
    summaryAtom,
    serverConfigsAtom,
  };
}

describe("environment shell projections", () => {
  it("projects corrupt catalog health as a configuration error and never confirms empty", () => {
    const catalogHealthAtom = Atom.make<ConnectionCatalogHealth>({
      status: "recovery-required",
      message: "The connection catalog needs recovery.",
    });
    const summaryAtom = createEnvironmentShellSummaryAtom({
      catalogValueAtom: Atom.make({ isReady: true, entries: new Map() }),
      catalogHealthAtom,
      shellStateValueAtom: Atom.family((_environmentId: EnvironmentId) =>
        Atom.make(shellState({ status: "live", updatedAt: "2026-07-01T00:00:00.000Z" })),
      ),
    });

    expect(AtomRegistry.make().get(summaryAtom)).toMatchObject({
      catalogHealth: {
        status: "recovery-required",
        message: "The connection catalog needs recovery.",
      },
      canShowEmptyProjects: false,
    });
  });

  it("maps structured supervisor failures without inspecting error text", () => {
    const blocked = (lastFailure: SupervisorConnectionState["lastFailure"]) => ({
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      network: "online" as const,
      phase: "blocked" as const,
      lastFailure,
    });
    const storageChanged = new ConnectionStorageChangedError({
      reason: "storage-changed",
      detail: "arbitrary storage copy",
      targetKey: "platform:primary",
      acceptedStorageInstanceId: "accepted",
      reportedStorageInstanceId: "reported",
    });
    const recoveryRequired = new ConnectionBlockedError({
      reason: "recovery-required",
      detail: "arbitrary recovery copy",
    });
    const configurationError = new ConnectionBlockedError({
      reason: "configuration",
      detail: "arbitrary configuration copy",
    });

    expect(
      resolveEnvironmentAvailabilityStatus({
        connection: blocked(storageChanged),
        snapshot: Option.none(),
        currentStatus: "starting",
      }),
    ).toBe("storage-changed");
    expect(
      resolveEnvironmentAvailabilityStatus({
        connection: blocked(recoveryRequired),
        snapshot: Option.none(),
        currentStatus: "starting",
      }),
    ).toBe("recovery-required");
    expect(
      resolveEnvironmentAvailabilityStatus({
        connection: blocked(configurationError),
        snapshot: Option.none(),
        currentStatus: "starting",
      }),
    ).toBe("configuration-error");
  });

  it("distinguishes cached degradation from unreachable no-snapshot state", () => {
    const connection: SupervisorConnectionState = {
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      network: "online",
      phase: "backoff",
    };
    const cached = shellState({
      status: "degraded",
      updatedAt: "2026-07-01T00:00:00.000Z",
    }).snapshot;

    expect(
      resolveEnvironmentAvailabilityStatus({
        connection,
        snapshot: cached,
        currentStatus: "live",
      }),
    ).toBe("degraded");
    expect(
      resolveEnvironmentAvailabilityStatus({
        connection,
        snapshot: Option.none(),
        currentStatus: "starting",
      }),
    ).toBe("unavailable");
  });

  it("treats configuring as starting without a snapshot and degraded with cached data", () => {
    const connection: SupervisorConnectionState = {
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      network: "online",
      phase: "connecting",
      stage: "configuring",
    };
    expect(
      resolveEnvironmentAvailabilityStatus({
        connection,
        snapshot: Option.none(),
        currentStatus: "starting",
      }),
    ).toBe("starting");
    expect(
      resolveEnvironmentAvailabilityStatus({
        connection,
        snapshot: shellState({ status: "live", updatedAt: "2026-07-01T00:00:00.000Z" }).snapshot,
        currentStatus: "live",
      }),
    ).toBe("degraded");
  });

  it("summarizes shell state and preserves identity when only irrelevant snapshot data changes", () => {
    const harness = makeHarness();
    const summary = harness.registry.get(harness.summaryAtom);

    expect(summary).toEqual({
      catalogReady: true,
      catalogHealth: { status: "ready" },
      desiredEnvironmentCount: 2,
      statuses: [
        {
          environmentId: ENVIRONMENT_ID,
          status: "degraded",
          hasSnapshot: true,
          error: null,
        },
        {
          environmentId: OTHER_ENVIRONMENT_ID,
          status: "synchronizing",
          hasSnapshot: true,
          error: "Retrying.",
        },
      ],
      canShowEmptyProjects: false,
      hasSnapshot: true,
      hasSynchronizingShell: true,
      hasCachedShell: true,
      hasLiveShell: false,
      firstError: "Retrying.",
      latestSnapshotUpdatedAt: "2026-06-02T00:00:00.000Z",
    });

    harness.registry.set(
      harness.shellStateAtom(ENVIRONMENT_ID),
      shellState({
        status: "degraded",
        updatedAt: "2026-06-01T00:00:00.000Z",
        snapshotSequence: 2,
      }),
    );

    expect(harness.registry.get(harness.summaryAtom)).toBe(summary);
  });

  it("allows empty projects only for a loaded non-empty catalog of live snapshots", () => {
    const harness = makeHarness();
    harness.registry.set(
      harness.shellStateAtom(ENVIRONMENT_ID),
      shellState({ status: "live", updatedAt: "2026-07-01T00:00:00.000Z" }),
    );
    harness.registry.set(
      harness.shellStateAtom(OTHER_ENVIRONMENT_ID),
      shellState({ status: "live", updatedAt: "2026-07-01T00:00:00.000Z" }),
    );
    expect(harness.registry.get(harness.summaryAtom).canShowEmptyProjects).toBe(true);
  });

  it.each([
    [false, new Map()],
    [true, new Map()],
  ] as const)(
    "does not authorize empty projects when catalog readiness is %s with zero desired environments",
    (isReady, entries) => {
      const summaryAtom = createEnvironmentShellSummaryAtom({
        catalogValueAtom: Atom.make({ isReady, entries }),
        shellStateValueAtom: Atom.family((_environmentId: EnvironmentId) =>
          Atom.make(shellState({ status: "live", updatedAt: "2026-07-01T00:00:00.000Z" })),
        ),
      });
      const summary = AtomRegistry.make().get(summaryAtom);
      expect(summary).toMatchObject({
        catalogReady: isReady,
        desiredEnvironmentCount: 0,
        statuses: [],
        canShowEmptyProjects: false,
      });
    },
  );

  it("prioritizes the first error and newest available snapshot", () => {
    const harness = makeHarness();
    harness.registry.set(
      harness.shellStateAtom(ENVIRONMENT_ID),
      shellState({
        status: "live",
        updatedAt: "2026-07-01T00:00:00.000Z",
        error: "Primary failed.",
      }),
    );
    harness.registry.set(
      harness.shellStateAtom(OTHER_ENVIRONMENT_ID),
      shellState({
        status: "degraded",
        updatedAt: "2026-06-01T00:00:00.000Z",
        error: "Secondary failed.",
      }),
    );

    expect(harness.registry.get(harness.summaryAtom)).toMatchObject({
      hasSnapshot: true,
      firstError: "Primary failed.",
      latestSnapshotUpdatedAt: "2026-07-01T00:00:00.000Z",
    });

    harness.registry.set(
      harness.shellStateAtom(OTHER_ENVIRONMENT_ID),
      shellState({ status: "unavailable", error: "Secondary failed." }),
    );
    expect(harness.registry.get(harness.summaryAtom).latestSnapshotUpdatedAt).toBe(
      "2026-07-01T00:00:00.000Z",
    );
  });

  it("preserves server-config map identity until a config reference changes", () => {
    const harness = makeHarness();
    const empty = harness.registry.get(harness.serverConfigsAtom);
    const config = { cwd: "/repo" } as ServerConfig;

    harness.registry.set(harness.configAtom(ENVIRONMENT_ID), config);
    const withConfig = harness.registry.get(harness.serverConfigsAtom);

    expect(withConfig).not.toBe(empty);
    expect(withConfig.get(ENVIRONMENT_ID)).toBe(config);

    harness.registry.set(harness.configAtom(ENVIRONMENT_ID), config);
    expect(harness.registry.get(harness.serverConfigsAtom)).toBe(withConfig);

    const replacement = { cwd: "/other" } as ServerConfig;
    harness.registry.set(harness.configAtom(ENVIRONMENT_ID), replacement);
    expect(harness.registry.get(harness.serverConfigsAtom).get(ENVIRONMENT_ID)).toBe(replacement);
  });
});
