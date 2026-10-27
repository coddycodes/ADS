import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  auth: {
    getToken: vi.fn(),
    isLoaded: true,
    isSignedIn: false,
    userId: null as string | null,
  },
  effects: [] as Array<() => void | (() => void)>,
  refSeeds: [] as unknown[],
  refIndex: 0,
  refs: [] as Array<{ current: unknown }>,
  removeEnvironments: vi.fn(),
  runtimeExit: vi.fn(),
  report: vi.fn(),
  setSession: vi.fn(),
  resolveOptions: vi.fn(() => ({ template: "relay" })),
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
  useRef: (initial: unknown) => {
    const index = harness.refIndex++;
    const ref = { current: index < harness.refSeeds.length ? harness.refSeeds[index] : initial };
    harness.refs[index] = ref;
    return ref;
  },
}));
vi.mock("@clerk/react", () => ({ useAuth: () => harness.auth }));
vi.mock("effect/Effect", () => ({
  flatMap: (callback: (client: unknown) => unknown) => ({ callback }),
}));
vi.mock("@coddy/client-runtime/relay", () => ({
  ManagedRelay: {
    ManagedRelayClient: {
      pipe: (operation: { callback: (client: unknown) => unknown }) =>
        operation.callback({ resetTokenCache: "reset-token-cache" }),
    },
  },
  setManagedRelaySession: (_registry: unknown, session: unknown) => harness.setSession(session),
}));
vi.mock("@coddy/client-runtime/state/runtime", () => ({
  reportAtomCommandResult: (result: unknown, options: unknown) => harness.report(result, options),
  settleAsyncResult: async (operation: () => Promise<unknown>) => operation(),
  settlePromise: async (operation: () => Promise<unknown>) => {
    try {
      return { _tag: "Success", value: await operation() };
    } catch (error) {
      return { _tag: "Failure", error };
    }
  },
}));
vi.mock("../connection/catalog", () => ({
  environmentCatalog: { removeRelayEnvironments: { label: "remove" } },
}));
vi.mock("../lib/runtime", () => ({
  runtime: { runPromiseExit: (operation: unknown) => harness.runtimeExit(operation) },
}));
vi.mock("../rpc/atomRegistry", () => ({ appAtomRegistry: { registry: true } }));
vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: () => harness.removeEnvironments,
}));
vi.mock("./publicConfig", () => ({
  resolveRelayClerkTokenOptions: () => harness.resolveOptions(),
}));

import {
  ManagedRelayAuthProvider,
  deactivateManagedRelayAuthentication,
  readManagedRelayClerkToken,
} from "./managedAuth";

function renderProvider(): React.ReactNode {
  harness.effects.length = 0;
  harness.refIndex = 0;
  harness.refs.length = 0;
  return ManagedRelayAuthProvider({ children: "Child" });
}

beforeEach(() => {
  deactivateManagedRelayAuthentication();
  harness.auth = {
    getToken: vi.fn().mockResolvedValue("token"),
    isLoaded: true,
    isSignedIn: false,
    userId: null,
  };
  harness.refSeeds = [];
  harness.removeEnvironments.mockReset();
  harness.removeEnvironments.mockResolvedValue({ _tag: "Success", value: undefined });
  harness.runtimeExit.mockReset();
  harness.runtimeExit.mockResolvedValue({ _tag: "Success", value: undefined });
  harness.report.mockReset();
  harness.setSession.mockClear();
  harness.resolveOptions.mockClear();
});

describe("ManagedRelayAuthProvider", () => {
  it("returns children and waits for Clerk to load", () => {
    harness.auth.isLoaded = false;
    expect(renderProvider()).toBe("Child");
    expect(harness.effects[0]?.()).toBeUndefined();
    expect(harness.removeEnvironments).not.toHaveBeenCalled();
  });

  it("cleans an unobserved signed-out account and reports both results", async () => {
    renderProvider();
    const cleanup = harness.effects[0]?.();
    await vi.waitFor(() => expect(harness.removeEnvironments).toHaveBeenCalledOnce());
    expect(harness.runtimeExit).toHaveBeenCalledWith("reset-token-cache");
    expect(harness.report).toHaveBeenCalledTimes(2);
    expect(harness.refs[0]?.current).toBeNull();
    if (typeof cleanup === "function") cleanup();

    harness.refSeeds = [null, null];
    renderProvider();
    harness.effects[0]?.();
    await Promise.resolve();
    expect(harness.removeEnvironments).toHaveBeenCalledOnce();
  });

  it("activates a new account and supplies Clerk tokens", async () => {
    harness.auth.isSignedIn = true;
    harness.auth.userId = "account-1";
    renderProvider();
    harness.effects[0]?.();
    await vi.waitFor(() =>
      expect(harness.setSession).toHaveBeenCalledWith(
        expect.objectContaining({ accountId: "account-1" }),
      ),
    );
    expect(await readManagedRelayClerkToken()).toBe("token");
    expect(harness.auth.getToken).toHaveBeenCalledWith({ template: "relay" });
    expect(harness.report).toHaveBeenCalledWith(expect.objectContaining({ _tag: "Success" }), {
      label: "cloud account activation",
    });
  });

  it("does not activate a session after the effect is cancelled", async () => {
    let finish: (() => void) | undefined;
    const transition = new Promise<void>((resolve) => {
      finish = resolve;
    });
    harness.auth.isSignedIn = true;
    harness.auth.userId = "account-cancelled";
    harness.refSeeds = [undefined, transition];
    renderProvider();
    const cleanup = harness.effects[0]?.();
    if (typeof cleanup === "function") cleanup();
    finish?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(harness.setSession).not.toHaveBeenCalledWith(
      expect.objectContaining({ accountId: "account-cancelled" }),
    );
  });

  it("queues cleanup before switching accounts", async () => {
    harness.auth.isSignedIn = true;
    harness.auth.userId = "account-2";
    harness.refSeeds = ["account-1", Promise.resolve()];
    renderProvider();
    harness.effects[0]?.();
    await vi.waitFor(() => expect(harness.removeEnvironments).toHaveBeenCalledOnce());
    await vi.waitFor(() =>
      expect(harness.setSession).toHaveBeenCalledWith(
        expect.objectContaining({ accountId: "account-2" }),
      ),
    );
  });

  it("keeps same-account activation behind an existing transition", async () => {
    harness.auth.isSignedIn = true;
    harness.auth.userId = "account-1";
    harness.refSeeds = ["account-1", Promise.resolve()];
    renderProvider();
    harness.effects[0]?.();
    await vi.waitFor(() =>
      expect(harness.setSession).toHaveBeenCalledWith(
        expect.objectContaining({ accountId: "account-1" }),
      ),
    );
    expect(harness.removeEnvironments).not.toHaveBeenCalled();
  });

  it("treats missing user ids as signed out and deactivates on unmount", async () => {
    harness.auth.isSignedIn = true;
    harness.auth.userId = null;
    harness.refSeeds = [null, null];
    renderProvider();
    harness.effects[0]?.();
    const unmount = harness.effects[1]?.();
    if (typeof unmount === "function") unmount();
    await Promise.resolve();
    expect(await readManagedRelayClerkToken()).toBeNull();
  });
});
