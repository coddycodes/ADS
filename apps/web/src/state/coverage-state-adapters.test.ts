import { DEFAULT_SERVER_SETTINGS, EnvironmentId } from "@coddy/contracts";
import { DEFAULT_RESOLVED_KEYBINDINGS } from "@coddy/shared/keybindings";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/reactivity";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

interface TestAtom {
  readonly key?: string;
  readonly read?: (get: (atom: TestAtom) => unknown) => unknown;
  pipe?: (...operations: Array<(atom: TestAtom) => TestAtom>) => TestAtom;
}

const h = vi.hoisted(() => ({
  atomValues: new Map<string, unknown>(),
  catalogAtom: { key: "catalog" },
  networkAtom: { key: "network" },
  preparedConnection: null as unknown,
  refresh: vi.fn(),
}));

function atomKey(atom: TestAtom): string | undefined {
  return atom.key;
}

function readAtom(atom: TestAtom): unknown {
  const key = atomKey(atom);
  if (key !== undefined && h.atomValues.has(key)) {
    return h.atomValues.get(key);
  }
  if (atom.read) {
    return atom.read((dependency) => readAtom(dependency));
  }
  return undefined;
}

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: <A>(callback: A) => callback,
    useEffect: (effect: () => void | (() => void)) => effect(),
    useMemo: <A>(factory: () => A) => factory(),
  };
});

vi.mock("effect/reactivity", async (importOriginal) => {
  const actual = await importOriginal<typeof import("effect/reactivity")>();
  const make = (value: unknown): TestAtom => {
    const atom: TestAtom = {
      read: typeof value === "function" ? (value as NonNullable<TestAtom["read"]>) : () => value,
      pipe: (...operations) => operations.reduce((current, operation) => operation(current), atom),
    };
    return atom;
  };
  return {
    ...actual,
    Atom: {
      ...actual.Atom,
      keepAlive: (atom: TestAtom) => atom,
      make,
      withLabel: (label: string) => (atom: TestAtom) => ({ ...atom, key: atom.key ?? label }),
    },
  };
});

vi.mock("@effect/atom-react", () => ({
  useAtomRefresh: () => h.refresh,
  useAtomValue: (atom: TestAtom) => readAtom(atom),
}));

vi.mock("../connection/catalog", () => ({
  environmentCatalog: {
    catalogValueAtom: h.catalogAtom,
    networkStatusValueAtom: h.networkAtom,
    stateAtom: (environmentId: string) => ({ key: `connection:${environmentId}` }),
  },
}));

vi.mock("../connection/runtime", () => ({ connectionAtomRuntime: {} }));

vi.mock("@coddy/client-runtime/state/server", () => ({
  createServerEnvironmentAtoms: () => ({
    configProjection: ({ environmentId }: { environmentId: string }) => ({
      key: `config-projection:${environmentId}`,
    }),
    configValueAtom: (environmentId: string) => ({ key: `config:${environmentId}` }),
    welcome: ({ environmentId }: { environmentId: string }) => ({
      key: `welcome:${environmentId}`,
    }),
  }),
}));

vi.mock("@coddy/client-runtime/state/shell", () => ({
  createEnvironmentServerConfigsAtom: () => ({ key: "environment-server-configs" }),
}));

vi.mock("@coddy/client-runtime/state/presentation", () => ({
  createEnvironmentPresentationAtoms: () => ({
    presentationAtom: (environmentId: string) => ({ key: `presentation:${environmentId}` }),
    presentationsAtom: { key: "presentations" },
  }),
}));

vi.mock("@coddy/client-runtime/connection", () => ({
  connectionCatalogDisplayUrl: (entry: { target: { httpBaseUrl?: string } }) =>
    entry.target.httpBaseUrl ?? null,
}));

vi.mock("@coddy/client-runtime/state/assets", () => ({
  resolveAssetUrl: (base: string, relative: string) => new URL(relative, base).toString(),
}));

vi.mock("../state/assets", () => ({
  assetEnvironment: {
    createUrl: () => ({ key: "asset-url" }),
    createUrls: () => ({ key: "asset-urls" }),
  },
}));

vi.mock("./session", () => ({
  environmentSession: { initialConfigValueAtom: {} },
  usePreparedConnection: () => h.preparedConnection,
}));

vi.mock("./relay", () => ({
  relayEnvironmentDiscovery: { stateValueAtom: { key: "relay-discovery" } },
}));

import { useAssetUrl, useAssetUrls } from "../assets/assetUrls";
import {
  useEnvironment,
  useEnvironmentConnectionState,
  useEnvironmentHttpBaseUrl,
  useEnvironments,
  usePrimaryEnvironment,
  usePrimaryEnvironmentId,
  useRelayEnvironmentDiscovery,
} from "./environments";
import { useEnvironmentPresentation } from "./presentation";
import { primaryEnvironmentIdAtom } from "./primaryEnvironment";
import { useEnvironmentQuery } from "./query";
import * as ServerState from "./server";

const environmentId = EnvironmentId.make("environment-local");

beforeEach(() => {
  h.atomValues.clear();
  h.atomValues.set("catalog", { isReady: false, entries: new Map() });
  h.atomValues.set("network", "online");
  h.preparedConnection = Option.none();
  h.refresh.mockReset();
});

describe("primary server state projections", () => {
  it("returns stable empty/default values without a primary environment", () => {
    expect(readAtom(primaryEnvironmentIdAtom as unknown as TestAtom)).toBeNull();
    expect(readAtom(ServerState.primaryServerStateAtom as unknown as TestAtom)).toEqual({
      config: null,
      latestEvent: null,
      welcome: null,
    });
    expect(readAtom(ServerState.primaryServerSettingsAtom as unknown as TestAtom)).toBe(
      DEFAULT_SERVER_SETTINGS,
    );
    expect(readAtom(ServerState.primaryServerProvidersAtom as unknown as TestAtom)).toEqual([]);
    expect(readAtom(ServerState.primaryServerKeybindingsAtom as unknown as TestAtom)).toBe(
      DEFAULT_RESOLVED_KEYBINDINGS,
    );
    expect(readAtom(ServerState.primaryServerAvailableEditorsAtom as unknown as TestAtom)).toEqual(
      [],
    );
    expect(
      readAtom(ServerState.primaryServerKeybindingsConfigPathAtom as unknown as TestAtom),
    ).toBeNull();
    expect(readAtom(ServerState.primaryServerObservabilityAtom as unknown as TestAtom)).toBeNull();
  });

  it("projects config, stream, welcome, and every configured derivative", () => {
    const config = {
      settings: { ...DEFAULT_SERVER_SETTINGS, theme: "dark" },
      providers: [{ id: "codex" }],
      keybindings: { commands: [] },
      availableEditors: ["vscode"],
      keybindingsConfigPath: "/repo/keybindings.json",
      observability: { enabled: true },
    };
    const latestEvent = { _tag: "Updated" };
    const welcome = { serverVersion: "1.0.0" };
    h.atomValues.set("catalog", {
      isReady: true,
      entries: new Map([
        [
          environmentId,
          {
            target: {
              _tag: "PrimaryConnectionTarget",
              label: "Local",
              httpBaseUrl: "http://127.0.0.1:4321",
            },
          },
        ],
      ]),
    });
    h.atomValues.set(`config:${environmentId}`, config);
    h.atomValues.set(`config-projection:${environmentId}`, AsyncResult.success({ latestEvent }));
    h.atomValues.set(`welcome:${environmentId}`, AsyncResult.success(welcome));

    expect(readAtom(primaryEnvironmentIdAtom as unknown as TestAtom)).toBe(environmentId);
    expect(readAtom(ServerState.primaryServerStateAtom as unknown as TestAtom)).toEqual({
      config,
      latestEvent,
      welcome,
    });
    expect(readAtom(ServerState.primaryServerConfigAtom as unknown as TestAtom)).toBe(config);
    expect(readAtom(ServerState.primaryServerConfigEventAtom as unknown as TestAtom)).toBe(
      latestEvent,
    );
    expect(readAtom(ServerState.primaryServerWelcomeAtom as unknown as TestAtom)).toBe(welcome);
    expect(readAtom(ServerState.primaryServerSettingsAtom as unknown as TestAtom)).toBe(
      config.settings,
    );
    expect(readAtom(ServerState.primaryServerProvidersAtom as unknown as TestAtom)).toBe(
      config.providers,
    );
    expect(readAtom(ServerState.primaryServerKeybindingsAtom as unknown as TestAtom)).toBe(
      config.keybindings,
    );
    expect(readAtom(ServerState.primaryServerAvailableEditorsAtom as unknown as TestAtom)).toBe(
      config.availableEditors,
    );
    expect(
      readAtom(ServerState.primaryServerKeybindingsConfigPathAtom as unknown as TestAtom),
    ).toBe("/repo/keybindings.json");
    expect(readAtom(ServerState.primaryServerObservabilityAtom as unknown as TestAtom)).toBe(
      config.observability,
    );

    h.atomValues.set(`config-projection:${environmentId}`, AsyncResult.initial(false));
    h.atomValues.set(`welcome:${environmentId}`, AsyncResult.initial(false));
    expect(readAtom(ServerState.primaryServerConfigEventAtom as unknown as TestAtom)).toBeNull();
    expect(readAtom(ServerState.primaryServerWelcomeAtom as unknown as TestAtom)).toBeNull();
  });
});

describe("environment presentation hooks", () => {
  it("projects catalogs, individual environments, primary ids, and relay discovery", () => {
    const primaryPresentation = {
      entry: {
        target: {
          _tag: "PrimaryConnectionTarget",
          label: "Local",
          httpBaseUrl: "http://127.0.0.1:4321",
        },
      },
      connection: { status: "available" },
    };
    const relayId = EnvironmentId.make("environment-relay");
    const relayPresentation = {
      entry: {
        target: {
          _tag: "RelayConnectionTarget",
          label: "Cloud",
        },
      },
      connection: { status: "available" },
    };
    const presentations = new Map([
      [environmentId, primaryPresentation],
      [relayId, relayPresentation],
    ]);
    h.atomValues.set("catalog", { isReady: true, entries: new Map() });
    h.atomValues.set("presentations", presentations);
    h.atomValues.set(`presentation:${environmentId}`, primaryPresentation);
    h.atomValues.set(`presentation:${relayId}`, relayPresentation);
    h.atomValues.set("relay-discovery", { _tag: "Ready", environments: [] });

    expect(useEnvironments()).toMatchObject({
      isReady: true,
      networkStatus: "online",
      environments: [
        { environmentId, label: "Local", relayManaged: false },
        { environmentId: relayId, label: "Cloud", relayManaged: true },
      ],
    });
    expect(useEnvironment(null)).toBeNull();
    expect(useEnvironment(environmentId)).toMatchObject({
      environmentId,
      label: "Local",
      displayUrl: "http://127.0.0.1:4321",
      relayManaged: false,
    });
    expect(useEnvironment(relayId)).toMatchObject({ relayManaged: true });
    expect(useRelayEnvironmentDiscovery()).toEqual({ _tag: "Ready", environments: [] });

    h.atomValues.set("catalog", {
      isReady: true,
      entries: new Map([[environmentId, primaryPresentation.entry]]),
    });
    expect(usePrimaryEnvironmentId()).toBe(environmentId);
    expect(usePrimaryEnvironment()).toMatchObject({ environmentId, label: "Local" });
    expect(useEnvironmentPresentation(null)).toEqual({ isReady: true, presentation: null });
    expect(useEnvironmentPresentation(environmentId)).toEqual({
      isReady: true,
      presentation: primaryPresentation,
    });
  });

  it("reads prepared base URLs and delegates connection state queries", () => {
    expect(useEnvironmentHttpBaseUrl(environmentId)).toBeNull();
    h.preparedConnection = Option.some({ httpBaseUrl: "https://relay.test/base/" });
    expect(useEnvironmentHttpBaseUrl(environmentId)).toBe("https://relay.test/base/");

    h.atomValues.set(`connection:${environmentId}`, AsyncResult.success("connected"));
    expect(useEnvironmentConnectionState(environmentId)).toMatchObject({
      data: "connected",
      error: null,
      isPending: false,
      revalidate: h.refresh,
    });
  });
});

describe("query and asset adapters", () => {
  it("formats query success, pending, Error, and non-Error failures", () => {
    expect(useEnvironmentQuery(null)).toMatchObject({
      data: null,
      error: null,
      isPending: false,
    });
    const queryAtom = { key: "query" };
    h.atomValues.set("query", { ...AsyncResult.success("value"), waiting: true });
    const pending = useEnvironmentQuery(queryAtom as never);
    expect(pending).toMatchObject({
      data: "value",
      error: null,
      isPending: true,
      revalidate: h.refresh,
      requiresRetry: false,
    });
    // An explicit Retry refreshes the same atom after clearing any cut-off latch.
    pending.refresh();
    expect(h.refresh).toHaveBeenCalledOnce();
    h.atomValues.set("query", AsyncResult.failure(Cause.fail(new Error("Request failed"))));
    expect(useEnvironmentQuery(queryAtom as never).error).toBe("Request failed");
    h.atomValues.set("query", AsyncResult.failure(Cause.fail(new Error("   "))));
    expect(useEnvironmentQuery(queryAtom as never).error).toBe("The environment request failed.");
    h.atomValues.set("query", AsyncResult.failure(Cause.fail("plain failure")));
    expect(useEnvironmentQuery(queryAtom as never).error).toBe("The environment request failed.");
  });

  it("resolves single and batched asset URLs across unavailable and failed states", () => {
    const resource = { _tag: "workspace-file", path: "index.html" } as never;
    h.atomValues.set("asset-url", AsyncResult.success({ relativeUrl: "/assets/index.html" }));
    h.atomValues.set("asset-urls", [
      AsyncResult.success({ relativeUrl: "/assets/index.html" }),
      AsyncResult.failure(Cause.fail("missing")),
    ]);
    expect(useAssetUrl(environmentId, resource)).toBeNull();
    expect(useAssetUrls(environmentId, [resource, resource])).toEqual([null, null]);

    h.preparedConnection = Option.some({ httpBaseUrl: "https://example.test/base/" });
    expect(useAssetUrl(environmentId, resource)).toBe("https://example.test/assets/index.html");
    expect(useAssetUrls(environmentId, [resource, resource])).toEqual([
      "https://example.test/assets/index.html",
      null,
    ]);
    h.atomValues.set("asset-url", AsyncResult.failure(Cause.fail("missing")));
    expect(useAssetUrl(environmentId, resource)).toBeNull();
  });
});
