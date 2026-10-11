import { EnvironmentId, PreviewGatewayError, ThreadId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const { readPreparedConnection, catalog } = vi.hoisted(() => ({
  readPreparedConnection: vi.fn(),
  catalog: { entries: new Map<string, unknown>() },
}));

vi.mock("~/state/session", () => ({ readPreparedConnection }));
vi.mock("~/connection/catalog", () => ({ environmentCatalog: { catalogValueAtom: "catalog" } }));
vi.mock("~/rpc/atomRegistry", () => ({ appAtomRegistry: { get: () => catalog } }));

const { showSamePortBusyNotice } = vi.hoisted(() => ({ showSamePortBusyNotice: vi.fn() }));
vi.mock("./linkNotices", () => ({ showSamePortBusyNotice }));

import {
  canonicalizePreviewUrl,
  type GatewayOpenMutation,
  isGatewayBootstrapUrl,
  isGatewayClientUrl,
  releasePreviewTab,
  releasePreviewTabAfterGrace,
  resetPreviewGatewayForTests,
  resolveForNavigation,
  retainPreviewTab,
  TAB_RELEASE_GRACE_MS,
  UNTRACKED_LEASE_MS,
} from "./previewGateway";

const environmentId = EnvironmentId.make("environment-1");
const threadId = ThreadId.make("thread-1");
const sshTarget = { alias: "box", hostname: "box.example", username: null, port: null };

const connection = (target: Record<string, unknown>, httpBaseUrl: string) => ({
  label: "Build box",
  httpBaseUrl,
  target,
});
const lan = () =>
  connection({ _tag: "BearerConnectionTarget", connectionId: "b:1" }, "http://192.168.1.5:3773");
const ssh = () =>
  connection({ _tag: "SshConnectionTarget", connectionId: "ssh:1" }, "http://127.0.0.1:45123");

type GatewayOpenResult = Awaited<ReturnType<GatewayOpenMutation>>;
const opened = (gatewayPort: number, capability = "CAP"): GatewayOpenResult =>
  AsyncResult.success({ gatewayPort, capability, expiresAtMs: 0 });
const refused = (reason: PreviewGatewayError["reason"]): GatewayOpenResult =>
  AsyncResult.failure(Cause.fail(new PreviewGatewayError({ reason, message: reason })));

let sshForward: ReturnType<typeof vi.fn>;
let releaseSshForward: ReturnType<typeof vi.fn>;

beforeEach(() => {
  resetPreviewGatewayForTests();
  readPreparedConnection.mockReset();
  showSamePortBusyNotice.mockReset();
  catalog.entries = new Map([
    [
      environmentId,
      {
        target: { _tag: "SshConnectionTarget" },
        profile: Option.some({ _tag: "SshConnectionProfile", target: sshTarget }),
      },
    ],
  ]);
  let nextLocalPort = 50_000;
  sshForward = vi.fn(async () => nextLocalPort++);
  releaseSshForward = vi.fn(async () => undefined);
  vi.stubGlobal("window", { desktopBridge: { preview: {}, sshForward, releaseSshForward } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("resolveForNavigation", () => {
  it("resolves LAN loopback through the gateway with a bootstrap url", async () => {
    readPreparedConnection.mockReturnValue(lan());
    const gatewayOpen = vi.fn(async () => opened(41000));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/app?key=1#h",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "ok",
      url: "http://192.168.1.5:41000/__bibcode/bootstrap?cap=CAP&to=%2Fapp%3Fkey%3D1%23h",
    });
    expect(gatewayOpen).toHaveBeenCalledWith({
      environmentId,
      input: { threadId, url: "http://localhost:5173/app?key=1#h" },
    });
    expect(sshForward).not.toHaveBeenCalled();
  });

  it("names this BiBCode page on the bootstrap url in browser mode so it may frame the preview", async () => {
    vi.stubGlobal("window", {});
    vi.stubGlobal("location", { origin: "http://192.168.1.5:3773", protocol: "http:" });
    readPreparedConnection.mockReturnValue(lan());

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen: vi.fn(async () => opened(41000)),
      }),
    ).resolves.toEqual({
      kind: "ok",
      url: "http://192.168.1.5:41000/__bibcode/bootstrap?cap=CAP&to=%2F&ui=http%3A%2F%2F192.168.1.5%3A3773",
    });
  });

  it("resolves SSH loopback through gateway plus local forward", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000, "C+/="));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://127.0.0.1:5173/",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "ok",
      url: "http://127.0.0.1:50000/__bibcode/bootstrap?cap=C%2B%2F%3D&to=%2F",
    });
    // The canonical URL the server sees is always on localhost.
    expect(gatewayOpen).toHaveBeenCalledWith({
      environmentId,
      input: { threadId, url: "http://localhost:5173/" },
    });
    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, 5173);
  });

  it("opens an SSH preview on the canonical origin when the same local port is free", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    sshForward.mockImplementationOnce(
      async (_target: unknown, _gatewayPort: number, preferred?: number) => preferred,
    );
    const gatewayOpen = vi.fn(async () => opened(41000));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://127.0.0.1:5173/cb?x=1",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "ok",
      url: "http://localhost:5173/__bibcode/bootstrap?cap=CAP&to=%2Fcb%3Fx%3D1",
    });
    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, 5173);
    // The real origin maps to itself, so the address bar and history keep it.
    expect(isGatewayClientUrl("http://localhost:5173/next")).toBe(true);
    expect(canonicalizePreviewUrl("http://localhost:5173/next")).toBe("http://localhost:5173/next");
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });

  it("frees a replaced listener's forward before asking for the same local port again", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    // The desktop: one forward per gateway port; a held local port is busy.
    const held = new Map<number, number>();
    sshForward.mockImplementation(
      async (_target: unknown, gatewayPort: number, preferred?: number) => {
        const live = held.get(gatewayPort);
        if (live !== undefined) return live;
        const local =
          preferred !== undefined && ![...held.values()].includes(preferred) ? preferred : 50_000;
        held.set(gatewayPort, local);
        return local;
      },
    );
    releaseSshForward.mockImplementation(async (_target: unknown, gatewayPort: number) => {
      held.delete(gatewayPort);
    });
    const open = (gatewayPort: number) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen: vi.fn(async () => opened(gatewayPort)),
        tabId: "tab-1",
      });

    await expect(open(41000)).resolves.toMatchObject({
      url: expect.stringMatching(/^http:\/\/localhost:5173\//),
    });
    // The server closed the idle listener and opened a new one.
    await expect(open(42000)).resolves.toMatchObject({
      url: expect.stringMatching(/^http:\/\/localhost:5173\//),
    });

    expect(releaseSshForward).toHaveBeenCalledWith(sshTarget, 41000);
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });

  it("shows the busy-port note once per environment and origin", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000));
    const open = (url: string) =>
      resolveForNavigation({ environmentId, threadId, canonicalUrl: url, gatewayOpen });

    await expect(open("http://localhost:5173/")).resolves.toMatchObject({
      kind: "ok",
      url: expect.stringMatching(/^http:\/\/127\.0\.0\.1:50000\//),
    });
    await open("http://localhost:5173/other");
    await open("http://localhost:8080/");

    expect(showSamePortBusyNotice).toHaveBeenCalledTimes(2);
    expect(showSamePortBusyNotice).toHaveBeenNthCalledWith(1, "localhost:5173");
    expect(showSamePortBusyNotice).toHaveBeenNthCalledWith(2, "localhost:8080");
  });

  it("sends no port preference for a port browsers refuse to load", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000));

    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:6000/",
      gatewayOpen,
    });

    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, undefined);
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });

  it("sends no port preference below 1024", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000));

    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost/",
      gatewayOpen,
    });

    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, undefined);
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });

  it("refuses SSH loopback without a desktop bridge before minting a capability", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    vi.stubGlobal("window", {});
    const gatewayOpen = vi.fn(async () => opened(41000));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen,
      }),
    ).resolves.toMatchObject({ kind: "unreachable" });
    expect(gatewayOpen).not.toHaveBeenCalled();
  });

  it("maps https rejection to the HTTPS copy", async () => {
    readPreparedConnection.mockReturnValue(lan());
    const gatewayOpen = vi.fn(async () => refused("https-unsupported"));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "https://localhost:8443/",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message:
        "HTTPS dev servers can't be previewed through the gateway yet; serve over HTTP or open it on Build box directly.",
      refusedByServer: true,
    });
  });

  it("maps a not-admitted rejection to the plain-HTTP-localhost copy", async () => {
    readPreparedConnection.mockReturnValue(lan());
    const gatewayOpen = vi.fn(async () => refused("not-admitted"));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message: "BiBCode can only preview plain HTTP addresses on Build box's own localhost.",
      refusedByServer: true,
    });
  });

  it("shows the server's not-reachable reason and where to open it instead", async () => {
    readPreparedConnection.mockReturnValue(lan());
    const notReachable = new PreviewGatewayError({
      reason: "not-reachable",
      message: "Previews aren't available on a public address.",
    });

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen: async () => AsyncResult.failure(Cause.fail(notReachable)),
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message: "Previews aren't available on a public address. Open it on Build box directly.",
    });
  });

  it("names a missing environment in lower case mid-sentence", async () => {
    // Reachability is classified with the connection, which then drops before the label is read.
    let calls = 0;
    readPreparedConnection.mockImplementation(() => (calls++ < 2 ? lan() : undefined));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen: async () => refused("not-admitted"),
      }),
    ).resolves.toMatchObject({
      message: "BiBCode can only preview plain HTTP addresses on this environment's own localhost.",
    });
  });

  it("keeps same-host loopback direct without calling gatewayOpen", async () => {
    readPreparedConnection.mockReturnValue(
      connection({ _tag: "PrimaryConnectionTarget" }, "http://127.0.0.1:3773"),
    );
    const gatewayOpen = vi.fn();

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/x",
        gatewayOpen,
      }),
    ).resolves.toEqual({ kind: "ok", url: "http://localhost:5173/x" });
    expect(gatewayOpen).not.toHaveBeenCalled();
  });

  it("reports relay environments as unreachable", async () => {
    readPreparedConnection.mockReturnValue(
      connection({ _tag: "RelayConnectionTarget" }, "https://abc.connect.example.com"),
    );
    const gatewayOpen = vi.fn();

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message:
        "This address is on Build box, not this computer. Opening its ports from here isn't supported yet.",
    });
    expect(gatewayOpen).not.toHaveBeenCalled();
  });
});

describe("canonicalizePreviewUrl", () => {
  it("canonicalizes gateway and bootstrap urls back to localhost", async () => {
    readPreparedConnection.mockReturnValue(lan());
    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      gatewayOpen: async () => opened(41000),
    });

    expect(
      canonicalizePreviewUrl(
        "http://192.168.1.5:41000/__bibcode/bootstrap?cap=CAP&to=%2Fapp%3Fkey%3D1%23h",
      ),
    ).toBe("http://localhost:5173/app?key=1#h");
    expect(canonicalizePreviewUrl("http://192.168.1.5:41000/docs?a=b#c")).toBe(
      "http://localhost:5173/docs?a=b#c",
    );
    expect(canonicalizePreviewUrl("https://example.com/x")).toBe("https://example.com/x");
    expect(canonicalizePreviewUrl("not a url")).toBe("not a url");
  });

  it("keeps shared state canonical across clients", async () => {
    readPreparedConnection.mockReturnValue(lan());
    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      gatewayOpen: async () => opened(41000),
    });
    readPreparedConnection.mockReturnValue(ssh());
    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      gatewayOpen: async () => opened(41001),
    });

    expect(canonicalizePreviewUrl("http://192.168.1.5:41000/a")).toBe("http://localhost:5173/a");
    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/b")).toBe("http://localhost:5173/b");
  });
});

describe("ssh forward bookkeeping", () => {
  it("releases the ssh forward when the last tab closes and on gateway port change", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const resolve = (tabId: string, gatewayPort: number, canonicalUrl = "http://localhost:5173/") =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl,
        tabId,
        gatewayOpen: async () => opened(gatewayPort),
      });

    for (let cycle = 0; cycle < 3; cycle += 1) {
      await resolve("tab-a", 41000);
      await resolve("tab-b", 41000);
      releasePreviewTab(environmentId, "tab-a");
      expect(releaseSshForward).toHaveBeenCalledTimes(cycle);
      releasePreviewTab(environmentId, "tab-b");
      expect(releaseSshForward).toHaveBeenCalledTimes(cycle + 1);
    }
    expect(releaseSshForward).toHaveBeenLastCalledWith(sshTarget, 41000);

    releaseSshForward.mockClear();
    await resolve("tab-a", 41000);
    await resolve("tab-a", 41002);
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    // A tab that moves to another upstream port keeps the old forward for its
    // Back history, and gives up both when it closes.
    await resolve("tab-a", 41003, "http://localhost:3000/");
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    releasePreviewTab(environmentId, "tab-a");
    expect(releaseSshForward.mock.calls).toEqual([
      [sshTarget, 41000],
      [sshTarget, 41002],
      [sshTarget, 41003],
    ]);
    releasePreviewTab(environmentId, "tab-a");
    expect(releaseSshForward).toHaveBeenCalledTimes(3);
  });

  it("keeps the current forward when a new address is refused", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-a",
      gatewayOpen: async () => opened(41000),
    });
    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "https://localhost:8443/",
        tabId: "tab-a",
        gatewayOpen: async () => refused("https-unsupported"),
      }),
    ).resolves.toMatchObject({ kind: "unreachable" });
    expect(releaseSshForward).not.toHaveBeenCalled();

    releasePreviewTab(environmentId, "tab-a");
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
  });

  it("keeps both forwards when one tab's navigations overlap", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    let finishFirst: (value: GatewayOpenResult) => void = () => undefined;
    const first = resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-a",
      gatewayOpen: () => new Promise((resolve) => (finishFirst = resolve)),
    });
    await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:3000/",
      tabId: "tab-a",
      gatewayOpen: async () => opened(42000),
    });
    finishFirst(opened(41000));
    await first;
    expect(releaseSshForward).not.toHaveBeenCalled();

    releasePreviewTab(environmentId, "tab-a");
    expect(releaseSshForward.mock.calls.map(([, port]) => port).toSorted()).toEqual([41000, 42000]);
  });

  it("does not let a failed resolution release a pending sibling's forward", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    let finishFirst: (value: GatewayOpenResult) => void = () => undefined;
    let finishSecond: (value: GatewayOpenResult) => void = () => undefined;
    const resolve = (gatewayOpen: GatewayOpenMutation) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        tabId: "tab-a",
        gatewayOpen,
      });
    const first = resolve(() => new Promise((done) => (finishFirst = done)));
    const second = resolve(() => new Promise((done) => (finishSecond = done)));
    finishFirst(refused("unavailable"));
    await first;
    finishSecond(opened(41000));
    await expect(second).resolves.toMatchObject({ kind: "ok" });
    expect(releaseSshForward).not.toHaveBeenCalled();

    releasePreviewTab(environmentId, "tab-a");
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
  });

  it("hands a closed tab's late forward to a pending peer", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const finishers: Array<(value: GatewayOpenResult) => void> = [];
    const resolve = (tabId: string) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        tabId,
        gatewayOpen: () => new Promise((done) => finishers.push(done)),
      });
    const a = resolve("tab-a");
    const b = resolve("tab-b");
    releasePreviewTab(environmentId, "tab-a");
    finishers[0]!(opened(41000));
    await a;
    finishers[1]!(opened(41000));
    await b;
    expect(releaseSshForward).not.toHaveBeenCalled();

    releasePreviewTab(environmentId, "tab-b");
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
  });

  it("keeps each thread's forward for the same upstream port", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    let finishOpen: (value: GatewayOpenResult) => void = () => undefined;
    const threadA = resolveForNavigation({
      environmentId,
      threadId: ThreadId.make("thread-a"),
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-a",
      gatewayOpen: () => new Promise((resolve) => (finishOpen = resolve)),
    });
    releasePreviewTab(environmentId, "tab-a");
    await resolveForNavigation({
      environmentId,
      threadId: ThreadId.make("thread-b"),
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-b",
      gatewayOpen: async () => opened(42000),
    });
    finishOpen(opened(41000));
    await threadA;

    // Thread A's late, orphaned forward goes; thread B's live one stays.
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
  });

  it("says when nothing is listening on the port", async () => {
    readPreparedConnection.mockReturnValue(lan());
    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen: async () => refused("no-upstream"),
      }),
    ).resolves.toEqual({
      kind: "unreachable",
      message: "Nothing is listening on port 5173 on Build box.",
      refusedByServer: true,
    });
  });

  it("offers retry or reconnect when the gateway is unavailable or the call fails", async () => {
    readPreparedConnection.mockReturnValue(lan());
    const retry =
      "Couldn't open a preview connection to Build box. Try again, or reconnect Build box if it keeps failing.";
    const resolveWith = (gatewayOpen: GatewayOpenMutation) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        gatewayOpen,
      });

    // "unavailable" (e.g. this client's expired session) stays out of shared state.
    await expect(resolveWith(async () => refused("unavailable"))).resolves.toEqual({
      kind: "unreachable",
      message: retry,
      retryable: true,
    });
    // A transport failure is this client's problem, not shared preview state.
    await expect(
      resolveWith(async (): Promise<GatewayOpenResult> =>
        AsyncResult.failure(Cause.fail(new Error("socket closed"))),
      ),
    ).resolves.toEqual({ kind: "unreachable", message: retry, retryable: true });
  });

  it("keeps a failed SSH forward local to this client", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const resolve = () =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        tabId: "tab-a",
        gatewayOpen: async () => opened(41000),
      });
    const disconnected = {
      kind: "unreachable",
      message: "Build box isn't connected. Reconnect it, then open the link again.",
      retryable: true,
    };
    // The Tauri bridge rejects with the bare string.
    sshForward.mockRejectedValueOnce("SSH connection is not active.");
    await expect(resolve()).resolves.toEqual(disconnected);
    sshForward.mockRejectedValueOnce(new Error("SSH connection is not active."));
    await expect(resolve()).resolves.toEqual(disconnected);

    // Any other rejection (forward budget, readiness timeout) is worth a retry.
    sshForward.mockRejectedValueOnce("SSH port forward did not become ready.");
    await expect(resolve()).resolves.toEqual({
      kind: "unreachable",
      message:
        "Couldn't open a preview connection to Build box. Try again, or reconnect Build box if it keeps failing.",
      retryable: true,
    });
    expect(releaseSshForward).not.toHaveBeenCalled();
  });

  it("forgets a released forward's client origin so a reused local port stays itself", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const result = await resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-a",
      gatewayOpen: async () => opened(41000),
    });
    expect(result).toMatchObject({ url: expect.stringMatching(/^http:\/\/127\.0\.0\.1:50000\//) });
    expect(isGatewayBootstrapUrl("http://127.0.0.1:50000/__bibcode/bootstrap?cap=C&to=%2F")).toBe(
      true,
    );
    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/x")).toBe("http://localhost:5173/x");

    releasePreviewTab(environmentId, "tab-a");

    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/x")).toBe("http://127.0.0.1:50000/x");
    expect(isGatewayBootstrapUrl("http://127.0.0.1:50000/__bibcode/bootstrap?cap=C&to=%2F")).toBe(
      false,
    );
  });

  it("keeps the mapping when a replacement forward reuses the old local port", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const resolve = (gatewayPort: number) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        tabId: "tab-a",
        gatewayOpen: async () => opened(gatewayPort),
      });
    await resolve(41000);
    // The server restarted the listener; the tunnel handed out the freed port again.
    sshForward.mockResolvedValueOnce(50000);
    await resolve(41002);

    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/x")).toBe("http://localhost:5173/x");
  });

  it("still canonicalizes Back to a replaced forward's origin until the tab is released", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const resolve = (gatewayPort: number) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://localhost:5173/",
        tabId: "tab-a",
        gatewayOpen: async () => opened(gatewayPort),
      });
    await resolve(41000); // local 50000
    // The listener was torn down and replaced; the new forward gets a new local port.
    await resolve(41002); // local 50001

    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    // History Back lands on the old local origin: shared state must stay canonical.
    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/back")).toBe(
      "http://localhost:5173/back",
    );
    expect(isGatewayClientUrl("http://127.0.0.1:50000/back")).toBe(true);

    releasePreviewTab(environmentId, "tab-a");
    expect(canonicalizePreviewUrl("http://127.0.0.1:50000/back")).toBe(
      "http://127.0.0.1:50000/back",
    );
    expect(canonicalizePreviewUrl("http://127.0.0.1:50001/x")).toBe("http://127.0.0.1:50001/x");
    expect(isGatewayClientUrl("http://127.0.0.1:50000/back")).toBe(false);
  });

  it("recognizes only known gateway bootstrap pages", () => {
    expect(isGatewayBootstrapUrl("http://localhost:5173/__bibcode/bootstrap?to=%2F")).toBe(false);
    expect(isGatewayBootstrapUrl("not a url")).toBe(false);
    expect(isGatewayBootstrapUrl(null)).toBe(false);
  });

  describe("leases", () => {
    /** A desktop bridge that, like the real one, spawns one ssh child per live gateway port. */
    function trackChildren() {
      const live = new Map<number, number>();
      let spawned = 0;
      let nextLocal = 60_000;
      sshForward.mockImplementation(async (_target: unknown, gatewayPort: number) => {
        if (!live.has(gatewayPort)) {
          spawned += 1;
          live.set(gatewayPort, nextLocal++);
        }
        return live.get(gatewayPort)!;
      });
      releaseSshForward.mockImplementation(async (_target: unknown, gatewayPort: number) => {
        live.delete(gatewayPort);
      });
      return { live, spawned: () => spawned };
    }
    const resolveTab = (tabId: string | undefined, gatewayPort = 41000, port = 5173) =>
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: `http://localhost:${port}/`,
        ...(tabId === undefined ? {} : { tabId }),
        gatewayOpen: async () => opened(gatewayPort),
      });

    beforeEach(() => {
      vi.useFakeTimers();
      readPreparedConnection.mockReturnValue(ssh());
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it("keeps a tab's forward through a tab switch and releases it after the grace period", async () => {
      const children = trackChildren();
      await resolveTab("tab-a");
      // Switch to tab B: A's host unmounts.
      releasePreviewTabAfterGrace(environmentId, "tab-a");
      await resolveTab("tab-b", 42000, 3000);
      vi.advanceTimersByTime(TAB_RELEASE_GRACE_MS - 1);
      // Back to tab A: its host mounts again and resolves its URL again.
      retainPreviewTab("tab-a");
      releasePreviewTabAfterGrace(environmentId, "tab-b");
      await resolveTab("tab-a");
      vi.advanceTimersByTime(TAB_RELEASE_GRACE_MS);

      expect(children.spawned()).toBe(2);
      expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 42000]]);

      releasePreviewTabAfterGrace(environmentId, "tab-a");
      vi.advanceTimersByTime(TAB_RELEASE_GRACE_MS);
      expect(releaseSshForward.mock.calls).toEqual([
        [sshTarget, 42000],
        [sshTarget, 41000],
      ]);
      expect(children.live.size).toBe(0);
    });

    it("releases a closed tab at once, without waiting for the grace period", async () => {
      await resolveTab("tab-a");
      releasePreviewTabAfterGrace(environmentId, "tab-a");
      releasePreviewTab(environmentId, "tab-a");
      expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);

      vi.advanceTimersByTime(TAB_RELEASE_GRACE_MS);
      expect(releaseSshForward).toHaveBeenCalledTimes(1);
    });

    it("leases an untracked open's forward for five minutes", async () => {
      await resolveTab(undefined);
      vi.advanceTimersByTime(UNTRACKED_LEASE_MS - 1);
      expect(releaseSshForward).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    });

    it("keeps a forward a tab still holds when an untracked lease on it expires", async () => {
      await resolveTab("tab-a");
      await resolveTab(undefined);
      vi.advanceTimersByTime(UNTRACKED_LEASE_MS);
      expect(releaseSshForward).not.toHaveBeenCalled();

      releasePreviewTab(environmentId, "tab-a");
      expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
    });

    it("releases every forward it spawned once no lease remains", async () => {
      const children = trackChildren();
      await resolveTab("tab-a");
      await resolveTab("tab-b");
      await resolveTab(undefined);
      await resolveTab("tab-a", 41005); // listener replaced
      await resolveTab(undefined, 43000, 8080);
      releasePreviewTabAfterGrace(environmentId, "tab-a");
      releasePreviewTab(environmentId, "tab-b");
      vi.advanceTimersByTime(UNTRACKED_LEASE_MS);

      expect(children.live.size).toBe(0);
      expect(releaseSshForward).toHaveBeenCalledTimes(children.spawned());
    });
  });

  it("releases a forward established after its tab already closed", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    let finishOpen: (value: ReturnType<typeof opened>) => void = () => undefined;
    const pending = resolveForNavigation({
      environmentId,
      threadId,
      canonicalUrl: "http://localhost:5173/",
      tabId: "tab-a",
      gatewayOpen: () => new Promise((resolve) => (finishOpen = resolve)),
    });
    releasePreviewTab(environmentId, "tab-a");
    finishOpen(opened(41000));
    await pending;

    expect(sshForward).toHaveBeenCalledTimes(1);
    expect(releaseSshForward.mock.calls).toEqual([[sshTarget, 41000]]);
  });
});
