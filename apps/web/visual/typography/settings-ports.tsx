import { RegistryContext } from "@effect/atom-react";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { PrimaryConnectionTarget } from "@bibcode/client-runtime/connection";
import type { AtomCommand } from "@bibcode/client-runtime/state/runtime";
import {
  AuthAccessWriteScope,
  AuthSessionId,
  DEFAULT_SERVER_SETTINGS,
  type AuthAccessStreamSnapshotEvent,
  type DesktopBridge,
  type DesktopServerExposureState,
} from "@bibcode/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import * as Cause from "effect/Cause";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { AsyncResult, Atom, AtomRegistry } from "effect/unstable/reactivity";
import { useEffect, useState, type ReactNode } from "react";

import type * as ServerPorts from "../../src/state/server";
import type * as EnvironmentPorts from "../../src/state/environments";
import type * as ShellPorts from "../../src/state/shell";
import type * as PreviewPorts from "../../src/state/preview";
import type * as AuthPorts from "../../src/state/auth";
import type * as SessionPorts from "../../src/environments/primary/sessionState";
import type { DesktopNetworkAccessSnapshot } from "../../src/state/desktopNetworkAccess";
import type { ThreadPreviewState } from "../../src/previewStateStore";
import { previewStateAtom } from "../../src/previewStateStore";
import { scopedThreadKey } from "@bibcode/client-runtime/environment";

import {
  fixtureEnvironmentId,
  fixtureThreadRef,
  providerInstance,
  liveProvider,
  keybindings,
  readAt,
  resourceProcesses,
  resourceHistory,
  traceDiagnostics,
} from "./settings-data";
import { gitServerConfigs } from "./git-server-config";
import { shareChangedSitesData } from "./share-changed-sites";

// These are nonvisual fixture service ports. Only the standalone gallery aliases
// the seven source modules named in coverage.json to this module.
type Server = typeof ServerPorts.serverEnvironment;
function refused<W, A, E>(label: string): AtomCommand<W, A, E> {
  return {
    label,
    run: async () => AsyncResult.failure(Cause.die(new Error(`Fixture forbids ${label}.`))),
  };
}

export const primaryServerSettingsAtom = Atom.make<
  Atom.Type<typeof ServerPorts.primaryServerSettingsAtom>
>({
  ...DEFAULT_SERVER_SETTINGS,
  providerInstances: { [liveProvider.instanceId]: providerInstance },
});
export const primaryServerProvidersAtom = Atom.make<
  Atom.Type<typeof ServerPorts.primaryServerProvidersAtom>
>([liveProvider]);
export const primaryServerKeybindingsAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerKeybindingsAtom>>(keybindings);
export const primaryServerKeybindingsConfigPathAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerKeybindingsConfigPathAtom>>(null);
export const primaryServerAvailableEditorsAtom = Atom.make<
  Atom.Type<typeof ServerPorts.primaryServerAvailableEditorsAtom>
>([]);
export const primaryServerObservabilityAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerObservabilityAtom>>(null);
export const primaryServerStateAtom = Atom.make<
  Atom.Type<typeof ServerPorts.primaryServerStateAtom>
>({ config: null, latestEvent: null, welcome: null });
export const primaryServerConfigAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerConfigAtom>>(null);
export const primaryServerConfigEventAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerConfigEventAtom>>(null);
export const primaryServerWelcomeAtom =
  Atom.make<Atom.Type<typeof ServerPorts.primaryServerWelcomeAtom>>(null);
export const environmentServerConfigsAtom =
  Atom.make<Atom.Type<typeof ServerPorts.environmentServerConfigsAtom>>(gitServerConfigs);
export const environmentSnapshotAtom: typeof ShellPorts.environmentSnapshotAtom = Atom.family(
  (_environmentId: Parameters<typeof ShellPorts.environmentSnapshotAtom>[0]) =>
    Atom.make<Atom.Type<ReturnType<typeof ShellPorts.environmentSnapshotAtom>>>(null),
);

const traceQuery = Atom.make(AsyncResult.success(traceDiagnostics));
const processesQuery = Atom.make(AsyncResult.success(resourceProcesses));
const usageQuery = Atom.make(AsyncResult.success({ readAt, isFetching: false, providers: [] }));
export const serverEnvironment: Pick<
  Server,
  | "traceDiagnostics"
  | "processDiagnostics"
  | "processResourceHistory"
  | "providerUsage"
  | "signalProcess"
  | "refreshProviders"
  | "refreshProviderUsage"
  | "updateProvider"
  | "upsertKeybinding"
  | "removeKeybinding"
  | "updateSettings"
> = {
  traceDiagnostics: () => traceQuery,
  processDiagnostics: () => processesQuery,
  processResourceHistory: Atom.family((target: Parameters<Server["processResourceHistory"]>[0]) =>
    Atom.make(
      AsyncResult.success({
        ...resourceHistory,
        windowMs: target.input.windowMs,
        bucketMs: target.input.bucketMs,
      }),
    ),
  ),
  providerUsage: () => usageQuery,
  signalProcess: refused("process signals"),
  refreshProviders: refused("provider probing"),
  refreshProviderUsage: refused("provider usage refresh"),
  updateProvider: refused("provider updates"),
  upsertKeybinding: refused("keybinding writes"),
  removeKeybinding: refused("keybinding removal"),
  updateSettings: refused("server settings writes"),
};

const ownedEnvironment: NonNullable<ReturnType<typeof EnvironmentPorts.usePrimaryEnvironment>> = {
  environmentId: fixtureEnvironmentId,
  label: "Owned typography environment",
  displayUrl: "http://example.invalid",
  relayManaged: false,
  entry: {
    target: new PrimaryConnectionTarget({
      environmentId: fixtureEnvironmentId,
      label: "Owned typography environment",
      httpBaseUrl: "http://example.invalid",
      wsBaseUrl: "ws://example.invalid",
    }),
    profile: Option.none(),
  },
  connection: { phase: "connected", error: null, traceId: null },
  serverConfig: null,
};
export const usePrimaryEnvironment: typeof EnvironmentPorts.usePrimaryEnvironment = () =>
  ownedEnvironment;
export const usePrimaryEnvironmentId: typeof EnvironmentPorts.usePrimaryEnvironmentId = () =>
  fixtureEnvironmentId;
export const useEnvironment: typeof EnvironmentPorts.useEnvironment = (environmentId) =>
  environmentId === fixtureEnvironmentId ? ownedEnvironment : null;
export const useEnvironmentHttpBaseUrl: typeof EnvironmentPorts.useEnvironmentHttpBaseUrl = () =>
  null;
export const usePrimaryLocalEnvironmentForSelected: typeof EnvironmentPorts.usePrimaryLocalEnvironmentForSelected =
  () => null;
export const selectPrimaryLocalEnvironmentId: typeof EnvironmentPorts.selectPrimaryLocalEnvironmentId =
  () => null;
export const useEnvironments: typeof EnvironmentPorts.useEnvironments = () => ({
  isReady: true,
  networkStatus: "online",
  environments: [ownedEnvironment],
  presentationById: new Map([[fixtureEnvironmentId, ownedEnvironment]]),
});
export const useRelayEnvironmentDiscovery: typeof EnvironmentPorts.useRelayEnvironmentDiscovery =
  () => {
    throw new Error("Fixture does not provide relay discovery.");
  };
export const useEnvironmentConnectionState: typeof EnvironmentPorts.useEnvironmentConnectionState =
  () => {
    throw new Error("Fixture does not provide a transport connection.");
  };

export const shellEnvironment: Pick<typeof ShellPorts.shellEnvironment, "openInEditor"> = {
  openInEditor: refused("editor launch"),
};

const previewList = Atom.make<Atom.Type<ReturnType<typeof PreviewPorts.previewEnvironment.list>>>(
  AsyncResult.initial(),
);
const previewEvents = Atom.make<
  Atom.Type<ReturnType<typeof PreviewPorts.previewEnvironment.events>>
>(AsyncResult.initial());
const discoveredServers: ReturnType<typeof PreviewPorts.previewEnvironment.discoveredServers> =
  Atom.make(AsyncResult.success({ servers: [], scannedAt: "2026-10-10T12:00:00.000Z" }));
export const previewEnvironment: Pick<
  typeof PreviewPorts.previewEnvironment,
  | "list"
  | "events"
  | "discoveredServers"
  | "open"
  | "navigate"
  | "resize"
  | "gatewayOpen"
  | "reportStatus"
> = {
  list: () => previewList,
  events: () => previewEvents,
  discoveredServers: () => discoveredServers,
  open: refused("preview opening"),
  navigate: refused("preview navigation"),
  resize: refused("preview resizing"),
  gatewayOpen: refused("preview gateway"),
  reportStatus: refused("preview status reporting"),
};

const expiration = DateTime.makeUnsafe("2030-01-01T00:00:00.000Z");
const shareAccess: AuthAccessStreamSnapshotEvent = {
  version: 1,
  revision: 1,
  type: "snapshot",
  payload: {
    pairingLinks: [
      {
        id: "fixture-link",
        credential: "INERT_COMPONENT_FIXTURE_NEVER_A_VALID_PAIRING_CREDENTIAL",
        scopes: [AuthAccessWriteScope],
        subject: "fixture-subject",
        label: "Fixture pairing link",
        createdAt: readAt,
        expiresAt: expiration,
        reach: "another-device",
      },
    ],
    clientSessions: [
      {
        sessionId: AuthSessionId.make("fixture-client"),
        subject: "fixture-subject",
        scopes: [AuthAccessWriteScope],
        method: "browser-session-cookie",
        client: {
          label: "Fixture desktop client",
          deviceType: "desktop",
          os: "Fixture OS",
          browser: "Fixture browser",
        },
        issuedAt: readAt,
        expiresAt: expiration,
        lastConnectedAt: readAt,
        connected: true,
        current: false,
        reach: "another-device",
      },
    ],
  },
};
let selectedShareAccess = shareAccess;
const accessQuery = Atom.make(() => AsyncResult.success(selectedShareAccess));
export const authEnvironment: Pick<typeof AuthPorts.authEnvironment, "accessChanges"> = {
  accessChanges: () => accessQuery,
};
export const usePrimarySessionState: typeof SessionPorts.usePrimarySessionState = () => ({
  data: null,
  error: null,
  isPending: false,
  refresh: () => undefined,
});
export const refreshPrimarySessionState: typeof SessionPorts.refreshPrimarySessionState = () =>
  undefined;

const exposure: DesktopServerExposureState = {
  configuredMode: "network-accessible",
  management: "native",
  mode: "network-accessible",
  endpointUrl: "http://192.0.2.20:3773",
  advertisedHost: "192.0.2.20",
  tailscaleServeEnabled: false,
  tailscaleServePort: 443,
};
export const ownedDesktopNetworkAccess: DesktopNetworkAccessSnapshot = {
  serverExposureState: exposure,
  advertisedEndpoints: [
    {
      id: "fixture-endpoint",
      label: "Fixture network endpoint",
      provider: { id: "desktop-core", label: "Desktop", kind: "core", isAddon: false },
      httpBaseUrl: "http://192.0.2.20:3773",
      wsBaseUrl: "ws://192.0.2.20:3773",
      reachability: "lan",
      compatibility: { hostedHttpsApp: "mixed-content-blocked", desktopApp: "compatible" },
      source: "desktop-core",
      status: "available",
      isDefault: true,
    },
  ],
};
export const ownedShareDesktopPort = {
  getServerExposureState: async () => selectedShareNetwork.serverExposureState,
  getAdvertisedEndpoints: async () => selectedShareNetwork.advertisedEndpoints,
  setTailscaleServeEnabled: async () => {
    throw new Error("Fixture forbids changing network exposure.");
  },
  getClientSettings: async () => DEFAULT_CLIENT_SETTINGS,
  setClientSettings: async () => {
    throw new Error("Fixture forbids host persistence.");
  },
} satisfies Pick<
  DesktopBridge,
  | "getServerExposureState"
  | "getAdvertisedEndpoints"
  | "setTailscaleServeEnabled"
  | "getClientSettings"
  | "setClientSettings"
>;
let selectedShareNetwork = ownedDesktopNetworkAccess;

// Call before mounting a ShareTab fixture. The port never crosses a native bridge.
export function prepareSettingsFixture(id: string): () => void {
  selectedShareAccess = shareAccess;
  selectedShareNetwork = ownedDesktopNetworkAccess;
  if (id === "settings-share-current-unreachable" || id === "settings-share-empty-access") {
    const selected = shareChangedSitesData(id);
    selectedShareAccess = selected.access;
    selectedShareNetwork = selected.network;
  } else if (id !== "settings-share-clients") return () => undefined;
  if (Object.getOwnPropertyDescriptor(window, "desktopBridge") !== undefined)
    throw new Error("Refusing to replace an existing desktop bridge.");
  Object.defineProperty(window, "desktopBridge", {
    configurable: true,
    value: ownedShareDesktopPort,
  });
  return () => {
    delete window.desktopBridge;
    selectedShareAccess = shareAccess;
    selectedShareNetwork = ownedDesktopNetworkAccess;
  };
}

function previewState(controller: "agent" | "human"): ThreadPreviewState {
  const snapshot: NonNullable<ThreadPreviewState["snapshot"]> = {
    threadId: fixtureThreadRef.threadId,
    tabId: "fixture-preview-tab",
    navStatus: { _tag: "Success", url: "https://example.invalid", title: "Owned fixture preview" },
    canGoBack: false,
    canGoForward: false,
    viewport: { _tag: "fill" },
    updatedAt: "2026-10-10T12:00:00.000Z",
  };
  const overlay: NonNullable<ThreadPreviewState["desktopOverlay"]> = {
    url: "https://example.invalid",
    canGoBack: false,
    canGoForward: false,
    loading: false,
    zoomFactor: 1,
    controller,
  };
  return {
    snapshot,
    sessions: { [snapshot.tabId]: snapshot },
    suppressedTabIds: new Set(),
    activeTabId: snapshot.tabId,
    desktopOverlay: overlay,
    desktopByTabId: { [snapshot.tabId]: overlay },
    localFailures: {},
    recentlySeenUrls: [],
  };
}

export function SettingsFixtureFrame({
  children,
  previewController,
}: {
  readonly children: ReactNode;
  readonly previewController?: "agent" | "human";
}) {
  const [registry] = useState(() => AtomRegistry.make());
  const [router] = useState(() => {
    if (previewController)
      registry.set(
        previewStateAtom(scopedThreadKey(fixtureThreadRef)),
        previewState(previewController),
      );
    const rootRoute = createRootRoute({ component: () => <>{children}</> });
    return createRouter({
      routeTree: rootRoute,
      history: createMemoryHistory({ initialEntries: ["/"] }),
    });
  });
  useEffect(() => () => registry.dispose(), [registry]);
  return (
    <RegistryContext.Provider value={registry}>
      <RouterProvider router={router} />
    </RegistryContext.Provider>
  );
}
