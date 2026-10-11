import { EnvironmentId } from "@bibcode/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const readPreparedConnection = vi.fn();

vi.mock("~/state/session", () => ({ readPreparedConnection }));

const env = EnvironmentId.make("environment-1");
const conn = (target: Record<string, unknown>, httpBaseUrl: string) => ({
  label: "Build box",
  httpBaseUrl,
  target,
});
const bearer = (httpBaseUrl: string) =>
  conn({ _tag: "BearerConnectionTarget", connectionId: "b:1" }, httpBaseUrl);

describe("browser target resolver", () => {
  beforeEach(() => readPreparedConnection.mockReset());
  afterEach(() => vi.unstubAllGlobals());

  it("keeps environment ports canonical on a private network host", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://192.168.1.25:3773"));
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    expect(
      resolveBrowserNavigationTarget(env, {
        kind: "environment-port",
        port: 5173,
        path: "/dashboard",
      }),
    ).toEqual({
      requestedUrl: "http://localhost:5173/dashboard",
      resolvedUrl: "http://localhost:5173/dashboard",
      resolutionKind: "direct",
      environmentId: "environment-1",
    });
  });

  it("keeps environment ports on localhost for a same-host environment", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://127.0.0.1:3773"),
    );
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    expect(
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 5173, path: "/x" }),
    ).toEqual({
      requestedUrl: "http://localhost:5173/x",
      resolvedUrl: "http://localhost:5173/x",
      resolutionKind: "direct",
      environmentId: "environment-1",
    });
  });

  it("frames a same-host server's localhost through its gateway in a browser tab", async () => {
    vi.stubGlobal("window", {});
    vi.stubGlobal("location", { protocol: "http:", hostname: "localhost" });
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://localhost:3773"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    // The environment's own address reaches its gateway; the literal loopback
    // stays as a direct open would load it.
    expect(resolvePreviewTarget(env, "http://127.0.0.2:5173/x")).toEqual({
      kind: "gateway",
      via: "host",
      host: "localhost",
      // Outside the frame (a new tab, no thread) it still opens directly.
      direct: true,
      url: "http://127.0.0.2:5173/x",
    });
    expect(resolvePreviewTarget(env, "http://0.0.0.0:5173/")).toMatchObject({
      url: "http://localhost:5173/",
    });
    // The gateway serves no HTTPS and admits no `*.localhost` name: those open directly.
    for (const url of ["https://localhost:5173/", "http://app.localhost:5173/"]) {
      expect(resolvePreviewTarget(env, url)).toEqual({ kind: "reachable", url });
    }
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://[::1]:4000"),
    );
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toMatchObject({ host: "::1" });
    // The server's own origin is already reachable.
    expect(resolvePreviewTarget(env, "http://[::1]:4000/a")).toEqual({
      kind: "reachable",
      url: "http://[::1]:4000/a",
    });
    vi.unstubAllGlobals();
  });

  it("refuses public hosts until the authenticated gateway exists", async () => {
    readPreparedConnection.mockReturnValue(bearer("https://203.0.113.10"));
    const { resolveBrowserNavigationTarget, UNREACHABLE_MESSAGES } =
      await import("./browserTargetResolver");
    expect(() =>
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 5173 }),
    ).toThrow(UNREACHABLE_MESSAGES["public-host"]("Build box"));
  });

  it("keeps environment-port automation targets canonical over SSH", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "SshConnectionTarget", connectionId: "ssh:1" }, "http://127.0.0.1:45123/"),
    );
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    expect(
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 5173 }).resolvedUrl,
    ).toBe("http://localhost:5173/");
  });

  it("normalizes schemeless localhost server-picker values", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://localhost:3773"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "localhost:5173")).toEqual({
      kind: "reachable",
      url: "http://localhost:5173/",
    });
    expect(resolvePreviewTarget(env, "0.0.0.0:3000/app")).toEqual({
      kind: "reachable",
      url: "http://localhost:3000/app",
    });
  });

  it("keeps normalized loopback URLs on a same-host primary environment", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://127.0.0.1:3773"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/a?b=1#c")).toEqual({
      kind: "reachable",
      url: "http://localhost:5173/a?b=1#c",
    });
    expect(resolvePreviewTarget(env, "localhost:5173/app")).toEqual({
      kind: "reachable",
      url: "http://localhost:5173/app",
    });
    expect(resolvePreviewTarget(env, "http://0.0.0.0:3000/")).toEqual({
      kind: "reachable",
      url: "http://localhost:3000/",
    });
  });

  it("routes a primary served from a remote LAN host through its gateway", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "PrimaryConnectionTarget" }, "http://192.168.1.25:3773"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toEqual({
      kind: "gateway",
      via: "host",
      host: "192.168.1.25",
      url: "http://localhost:5173/",
    });
  });

  it("treats 0.0.0.0 as loopback on a LAN bearer environment", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://192.168.1.25:3773"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://0.0.0.0:3000/")).toEqual({
      kind: "gateway",
      via: "host",
      host: "192.168.1.25",
      url: "http://localhost:3000/",
    });
  });

  it("reaches tailnet addresses through the gateway", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://100.64.0.10:3773"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toEqual({
      kind: "gateway",
      via: "host",
      host: "100.64.0.10",
      url: "http://localhost:5173/",
    });
  });

  it("classifies SSH by target tag even with a loopback httpBaseUrl", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "SshConnectionTarget", connectionId: "ssh:1" }, "http://127.0.0.1:45123/"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toEqual({
      kind: "gateway",
      via: "ssh",
      url: "http://localhost:5173/",
    });
  });

  it("keeps URLs on the server's own forwarded origin reachable over SSH", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "SshConnectionTarget", connectionId: "ssh:1" }, "http://127.0.0.1:45123/"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://127.0.0.1:45123/api/assets/a.html#top")).toEqual({
      kind: "reachable",
      url: "http://127.0.0.1:45123/api/assets/a.html#top",
    });
  });

  it("refuses relay environments", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "RelayConnectionTarget" }, "https://abc.connect.example.com"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://127.0.0.1:8080/")).toMatchObject({
      kind: "unreachable",
      reason: "relay",
    });
  });

  it("refuses bearer environments on public IP addresses", async () => {
    readPreparedConnection.mockReturnValue(bearer("https://203.0.113.10"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toMatchObject({
      kind: "unreachable",
      reason: "public-host",
    });
  });

  it("routes desktop-local WSL loopback through the gateway on the VM address", async () => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "BearerConnectionTarget", connectionId: "local:wsl" }, "http://172.22.1.5:3773"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/x")).toEqual({
      kind: "gateway",
      via: "host",
      host: "172.22.1.5",
      url: "http://localhost:5173/x",
    });
  });

  it("reports a disconnected environment instead of guessing", async () => {
    readPreparedConnection.mockReturnValue(null);
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toEqual({
      kind: "unreachable",
      reason: "disconnected",
      environmentLabel: "This environment",
      url: "http://localhost:5173/",
    });
    expect(resolvePreviewTarget(env, "localhost:3000/path")).toMatchObject({
      kind: "unreachable",
      reason: "disconnected",
    });
  });

  it("passes non-loopback URLs through without reading the connection", async () => {
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "https://example.com/docs")).toEqual({
      kind: "reachable",
      url: "https://example.com/docs",
    });
    expect(resolvePreviewTarget(env, "example.com/app")).toEqual({
      kind: "reachable",
      url: "https://example.com/app",
    });
    expect(readPreparedConnection).not.toHaveBeenCalled();
  });

  it("routes private IPv6 environment hosts through the gateway", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://[fd12::5]:3773"));
    const { resolveBrowserNavigationTarget, resolvePreviewTarget } =
      await import("./browserTargetResolver");
    expect(
      resolveBrowserNavigationTarget(env, {
        kind: "environment-port",
        port: 5173,
        path: "/app?mode=test",
      }).resolvedUrl,
    ).toBe("http://localhost:5173/app?mode=test");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toEqual({
      kind: "gateway",
      via: "host",
      host: "fd12::5",
      url: "http://localhost:5173/",
    });
  });

  it("leaves malformed input for the normal navigation error path", async () => {
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "   ")).toEqual({ kind: "reachable", url: "   " });
  });

  it("returns direct URL targets without reading connection state", async () => {
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    expect(
      resolveBrowserNavigationTarget(env, { kind: "url", url: "https://example.test/docs" }),
    ).toEqual({
      requestedUrl: "https://example.test/docs",
      resolvedUrl: "https://example.test/docs",
      resolutionKind: "direct",
      environmentId: "environment-1",
    });
    expect(readPreparedConnection).not.toHaveBeenCalled();
  });

  it("rejects disconnected environment ports", async () => {
    readPreparedConnection.mockReturnValue(null);
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    expect(() =>
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 3000 }),
    ).toThrow("isn't connected");
  });

  it.each([
    "http://localhost:4321",
    "http://machine.local:4321",
    "http://machine.ts.net:4321",
    "http://10.0.0.1:4321",
    "http://172.16.0.1:4321",
    "http://172.31.255.1:4321",
    "http://192.168.1.1:4321",
    "http://100.64.0.1:4321",
    "http://100.127.255.1:4321",
    "http://127.1.2.3:4321",
    "http://169.254.2.3:4321",
    "http://[fd00::1]:4321",
    "http://[fe80::1]:4321",
    "http://devbox:4321",
    "http://build.lan:4321",
    "http://box.home.arpa:4321",
    "http://1.example.test:4321",
  ])("accepts private environment host %s", async (httpBaseUrl) => {
    readPreparedConnection.mockReturnValue(bearer(httpBaseUrl));
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    const result = resolveBrowserNavigationTarget(env, {
      kind: "environment-port",
      port: 8443,
      protocol: "https",
      path: "health",
    });
    expect(result.resolvedUrl).toContain(":8443/health");
    expect(result.requestedUrl).toBe("https://localhost:8443/health");
  });

  it.each([
    "http://172.15.0.1:4321",
    "http://172.32.0.1:4321",
    "http://192.167.1.1:4321",
    "http://100.63.0.1:4321",
    "http://100.128.0.1:4321",
    "http://[2001:db8::1]:4321",
    "http://1.2.3:4321",
    "http://8.8.8.8:4321",
  ])("rejects non-private environment host %s", async (httpBaseUrl) => {
    readPreparedConnection.mockReturnValue(bearer(httpBaseUrl));
    const { resolveBrowserNavigationTarget, UNREACHABLE_MESSAGES } =
      await import("./browserTargetResolver");
    expect(() =>
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 3000 }),
    ).toThrow(UNREACHABLE_MESSAGES["public-host"]("Build box"));
  });

  it("keeps the scheme of wildcard HTTPS discovery so the gateway can refuse it", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://10.0.0.2:4321"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "https://0.0.0.0/path")).toEqual({
      kind: "gateway",
      via: "host",
      host: "10.0.0.2",
      url: "https://localhost/path",
    });
  });

  it("routes loopback on a non-IP environment host name through the gateway", async () => {
    readPreparedConnection.mockReturnValue(bearer("http://devbox:3773"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173")).toEqual({
      kind: "gateway",
      via: "host",
      host: "devbox",
      url: "http://localhost:5173/",
    });
  });

  const loopbackForms = [
    ["http://[::]:8000/x", "http://localhost:8000/x"],
    ["http://0.0.0.0:8000/x", "http://localhost:8000/x"],
    ["http://[::1]:8000/x", "http://[::1]:8000/x"],
    ["http://127.0.0.2:8000/x", "http://127.0.0.2:8000/x"],
    ["http://127.1.2.3:8000/x", "http://127.1.2.3:8000/x"],
    ["http://app.localhost:8000/x", "http://app.localhost:8000/x"],
    ["http://LOCALHOST:8000/x", "http://localhost:8000/x"],
  ] as const;

  it.each(loopbackForms)(
    "keeps %s on this machine for a same-host environment",
    async (raw, want) => {
      readPreparedConnection.mockReturnValue(
        conn({ _tag: "PrimaryConnectionTarget" }, "http://127.0.0.1:3773"),
      );
      const { resolvePreviewTarget } = await import("./browserTargetResolver");
      expect(resolvePreviewTarget(env, raw)).toEqual({ kind: "reachable", url: want });
    },
  );

  it.each(loopbackForms)("canonicalizes %s for a LAN environment gateway", async (raw) => {
    readPreparedConnection.mockReturnValue(bearer("http://192.168.1.25:3773"));
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, raw)).toEqual({
      kind: "gateway",
      via: "host",
      host: "192.168.1.25",
      url: "http://localhost:8000/x",
    });
  });

  it.each(loopbackForms)("canonicalizes %s for an SSH gateway", async (raw) => {
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "SshConnectionTarget", connectionId: "ssh:1" }, "http://127.0.0.1:45123/"),
    );
    const { resolvePreviewTarget } = await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, raw)).toEqual({
      kind: "gateway",
      via: "ssh",
      url: "http://localhost:8000/x",
    });
  });

  it("treats a malformed connection base URL as disconnected instead of throwing", async () => {
    readPreparedConnection.mockReturnValue(bearer("not a url"));
    const { resolvePreviewTarget, resolveBrowserNavigationTarget } =
      await import("./browserTargetResolver");
    expect(resolvePreviewTarget(env, "http://localhost:5173/")).toMatchObject({
      kind: "unreachable",
      reason: "disconnected",
    });
    expect(() =>
      resolveBrowserNavigationTarget(env, { kind: "environment-port", port: 5173 }),
    ).toThrow("isn't connected");
  });

  it("resolves agent url targets through the environment topology", async () => {
    const { resolveBrowserNavigationTarget } = await import("./browserTargetResolver");
    readPreparedConnection.mockReturnValue(
      conn({ _tag: "RelayConnectionTarget" }, "https://abc.connect.example.com"),
    );
    expect(() =>
      resolveBrowserNavigationTarget(env, { kind: "url", url: "http://localhost:3000" }),
    ).toThrow("This address is on Build box, not this computer.");

    // Gateway-backed targets stay canonical; each client resolves them for its own view.
    readPreparedConnection.mockReturnValue(bearer("http://192.168.1.25:3773"));
    expect(
      resolveBrowserNavigationTarget(env, { kind: "url", url: "http://0.0.0.0:3000/app" }),
    ).toEqual({
      requestedUrl: "http://0.0.0.0:3000/app",
      resolvedUrl: "http://localhost:3000/app",
      resolutionKind: "direct",
      environmentId: "environment-1",
    });
    expect(
      resolveBrowserNavigationTarget(env, { kind: "url", url: "https://example.test/docs" }),
    ).toMatchObject({ resolvedUrl: "https://example.test/docs", resolutionKind: "direct" });
  });

  it("names the environment without transport jargon", async () => {
    const { UNREACHABLE_MESSAGES } = await import("./browserTargetResolver");
    for (const reason of ["ssh", "relay", "public-host"] as const) {
      expect(UNREACHABLE_MESSAGES[reason]("Box")).toBe(
        "This address is on Box, not this computer. Opening its ports from here isn't supported yet.",
      );
    }
    expect(UNREACHABLE_MESSAGES.disconnected("Box")).toBe(
      "Box isn't connected. Reconnect it, then open the link again.",
    );
  });
});
