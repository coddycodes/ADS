import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  session: null as null | { accountId: string },
  relayResult: { _tag: "Success", value: [] } as unknown,
  relaySnapshot: {
    data: [] as unknown[],
    error: null as string | null,
    errorTraceId: null as string | null,
    isPending: false,
  },
  primary: null as unknown,
  primaryResult: { _tag: "Success", value: null, waiting: false } as unknown,
  refreshEnvironments: vi.fn(),
  environmentsAtom: vi.fn((accountId: string) => ({ key: `relay:${accountId}` })),
  registryGet: vi.fn(),
  registryRefresh: vi.fn(),
}));

interface FakeAtom {
  readonly key?: string;
  pipe(...operations: Array<(atom: FakeAtom) => FakeAtom>): FakeAtom;
}

function fakeAtom(key?: string): FakeAtom {
  const atom: FakeAtom = {
    ...(key ? { key } : {}),
    pipe: (...operations) => operations.reduce((current, operation) => operation(current), atom),
  };
  return atom;
}

vi.mock("react", () => ({
  useCallback: <A>(callback: A) => callback,
  useEffect: (effect: () => void) => effect(),
  useMemo: <A>(factory: () => A) => factory(),
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: { key?: string }) => {
    if (atom.key === "managed-session") return h.session;
    if (atom.key?.startsWith("relay:")) return h.relayResult;
    if (atom.key?.startsWith("primary-cloud:")) return h.primaryResult;
    return atom.key === "relay-empty" ? { _tag: "Success", value: [] } : h.primaryResult;
  },
}));

vi.mock("effect/reactivity", () => ({
  AsyncResult: {
    success: (value: unknown) => ({ _tag: "Success", value, waiting: false }),
    value: (result: { _tag: string; value?: unknown }) =>
      result._tag === "Success" ? Option.some(result.value) : Option.none(),
  },
  Atom: {
    family: (create: (key: string) => FakeAtom) => {
      const cache = new Map<string, FakeAtom>();
      return (key: string) => {
        const existing = cache.get(key);
        if (existing) return existing;
        const atom = create(key);
        cache.set(key, atom);
        return atom;
      };
    },
    keepAlive: (atom: FakeAtom) => atom,
    make: (value: unknown) =>
      fakeAtom(
        Array.isArray((value as { value?: unknown })?.value) ? "relay-empty" : "primary-empty",
      ),
    runtime: () => ({ atom: (value: { key?: string }) => fakeAtom(value.key) }),
    setIdleTTL: () => (atom: FakeAtom) => atom,
    swr: () => (atom: FakeAtom) => atom,
    withLabel: (label: string) => (atom: FakeAtom) => fakeAtom(atom.key ?? label),
  },
}));

vi.mock("effect/Context", () => ({ get: () => ({}) }));
vi.mock("effect/Effect", () => ({ map: () => (value: unknown) => value }));
vi.mock("effect/Layer", () => ({ effect: () => ({}) }));
vi.mock("effect/http", () => ({ HttpClient: { HttpClient: {} } }));

vi.mock("@coddy/client-runtime/relay", () => ({
  ManagedRelay: { ManagedRelayClient: {} },
  managedRelaySessionAtom: { key: "managed-session" },
  createManagedRelayQueryManager: () => ({
    environmentsAtom: h.environmentsAtom,
    refreshEnvironments: h.refreshEnvironments,
  }),
  readManagedRelaySnapshotState: () => h.relaySnapshot,
}));

vi.mock("../lib/runtime", () => ({
  runtime: { contextEffect: { pipe: () => ({}) } },
}));

vi.mock("../rpc/atomRegistry", () => ({
  appAtomRegistry: {
    get: h.registryGet,
    refresh: h.registryRefresh,
  },
}));

vi.mock("../state/environments", () => ({
  usePrimaryEnvironment: () => h.primary,
}));

vi.mock("./linkEnvironment", () => ({
  readPrimaryCloudLinkState: ({ target }: { target: { environmentId: string } }) => ({
    key: `primary-cloud:${target.environmentId}`,
  }),
}));

import { refreshManagedRelayEnvironments, useManagedRelayEnvironments } from "./managedRelayState";
import { refreshPrimaryCloudLinkState, usePrimaryCloudLinkState } from "./primaryCloudLinkState";

beforeEach(() => {
  h.session = null;
  h.relayResult = { _tag: "Success", value: [] };
  h.relaySnapshot = { data: [], error: null, errorTraceId: null, isPending: false };
  h.primary = null;
  h.primaryResult = { _tag: "Success", value: null, waiting: false };
  h.refreshEnvironments.mockReset();
  h.environmentsAtom.mockClear();
  h.registryGet.mockReset().mockReturnValue(null);
  h.registryRefresh.mockReset();
});

describe("managed relay cloud state", () => {
  it("returns the empty account view and keeps accountless refreshes inert", () => {
    const state = useManagedRelayEnvironments();
    expect(state).toMatchObject({ accountId: null, data: [], error: null });
    state.refresh();
    refreshManagedRelayEnvironments();
    expect(h.refreshEnvironments).not.toHaveBeenCalled();
  });

  it("selects, refreshes, and reports the signed-in account query", () => {
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    h.session = { accountId: "account-1" };
    h.registryGet.mockReturnValue(h.session);
    h.relaySnapshot = {
      data: [],
      error: "relay unavailable",
      errorTraceId: "trace-1",
      isPending: false,
    };

    const state = useManagedRelayEnvironments();
    expect(h.environmentsAtom).toHaveBeenCalledWith("account-1");
    expect(state.accountId).toBe("account-1");
    expect(errorLog).toHaveBeenCalledWith("[coddy-studio-cloud] Relay environment listing failed", {
      message: "relay unavailable",
      traceId: "trace-1",
    });
    state.refresh();
    refreshManagedRelayEnvironments();
    expect(h.refreshEnvironments).toHaveBeenCalledTimes(2);
  });
});

describe("primary cloud link state", () => {
  it("returns an idle null target and ignores null refreshes", () => {
    const state = usePrimaryCloudLinkState();
    expect(state).toMatchObject({ data: null, error: null, isPending: false, target: null });
    state.refresh();
    refreshPrimaryCloudLinkState(null);
    expect(h.registryRefresh).not.toHaveBeenCalled();
  });

  it("projects a primary target, data, pending state, and direct refresh", () => {
    h.primary = {
      environmentId: "environment-1",
      label: "Local",
      entry: {
        target: {
          _tag: "PrimaryConnectionTarget",
          httpBaseUrl: "http://127.0.0.1:4321",
          wsBaseUrl: "ws://127.0.0.1:4321",
        },
      },
    };
    h.primaryResult = { _tag: "Success", value: { linked: true }, waiting: true };

    const state = usePrimaryCloudLinkState();
    expect(state).toMatchObject({
      data: { linked: true },
      error: null,
      isPending: true,
      target: {
        environmentId: "environment-1",
        label: "Local",
        httpBaseUrl: "http://127.0.0.1:4321",
        wsBaseUrl: "ws://127.0.0.1:4321",
      },
    });
    state.refresh();
    expect(h.registryRefresh).toHaveBeenCalledOnce();
  });

  it("formats Error and non-Error query failures", () => {
    h.primary = {
      environmentId: "environment-1",
      label: "Remote",
      entry: { target: { _tag: "RelayConnectionTarget" } },
    };
    expect(usePrimaryCloudLinkState().target).toBeNull();

    h.primary = {
      environmentId: "environment-1",
      label: "Local",
      entry: {
        target: {
          _tag: "PrimaryConnectionTarget",
          httpBaseUrl: "http://127.0.0.1:4321",
          wsBaseUrl: "ws://127.0.0.1:4321",
        },
      },
    };
    h.primaryResult = {
      _tag: "Failure",
      cause: Cause.fail(new Error("link failed")),
      waiting: false,
    };
    expect(usePrimaryCloudLinkState().error).toBe("link failed");
    h.primaryResult = { _tag: "Failure", cause: Cause.fail("unknown"), waiting: false };
    expect(usePrimaryCloudLinkState().error).toBe("Could not read Coddy Connect link state.");
  });
});
