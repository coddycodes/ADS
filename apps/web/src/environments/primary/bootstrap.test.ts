import {
  PrimaryConnectionRegistration,
  PrimaryConnectionTarget,
} from "@coddy/client-runtime/connection";
import {
  EnvironmentId,
  type DesktopEnvironmentBootstrap,
  type ExecutionEnvironmentDescriptor,
} from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import * as Effect from "effect/Effect";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  getPrimaryKnownEnvironment,
  isDesktopEnvironmentBootstrapIncompleteError,
  isPrimaryEnvironmentProtocolUnsupportedError,
  isPrimaryEnvironmentUrlInvalidError,
  readPrimaryEnvironmentTarget,
  resolvePrimaryEnvironmentHttpUrl,
  resolveInitialPrimaryEnvironmentDescriptor,
  resetPrimaryEnvironmentDescriptorForTests,
  writePrimaryEnvironmentDescriptor,
} from ".";
import { installEnvironmentHttpTest } from "../../../test/environmentHttpTest";
import {
  canReuseCachedPlatformRegistration,
  primaryRegistrationToRetainAfterTopologyRead,
  readPrimaryEnvironmentTargetResult,
} from "../../connection/platform";
import { isDesktopPrimaryEnvironmentWithheldError } from "./target";

const BASE_ENVIRONMENT = {
  environmentId: EnvironmentId.make("environment-local"),
  label: "Local environment",
  platform: {
    os: "darwin",
    arch: "arm64",
  },
  serverVersion: "0.0.0-test",
  storageInstanceId: null,
  bootId: null,
  remoteUpdateSupport: null,
  remoteProtocolVersion: 1,
  minCompatibleRemoteProtocol: 1,
  capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
} satisfies ExecutionEnvironmentDescriptor;

let disposeHttpTest: (() => Promise<void>) | undefined;

async function installDescriptorApi() {
  const testApi = await installEnvironmentHttpTest({
    descriptor: () => Effect.succeed(BASE_ENVIRONMENT),
  });
  disposeHttpTest = testApi.dispose;
  return testApi;
}

function installTestBrowser(url: string) {
  vi.stubGlobal("window", {
    location: new URL(url),
    history: {
      replaceState: vi.fn(),
    },
  });
}

function captureThrown(run: () => unknown): unknown {
  try {
    run();
  } catch (error) {
    return error;
  }
  throw new Error("Expected the operation to throw.");
}

describe("environmentBootstrap", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
    installTestBrowser("http://localhost/");
  });

  afterEach(async () => {
    await disposeHttpTest?.();
    disposeHttpTest = undefined;
    resetPrimaryEnvironmentDescriptorForTests();
    vi.unstubAllEnvs();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("attaches the bootstrapped environment descriptor to the primary environment", () => {
    vi.stubGlobal("window", {
      location: {
        origin: "http://localhost:3773",
      },
      desktopBridge: undefined,
    });
    const descriptor = {
      environmentId: EnvironmentId.make("environment-local"),
      label: "Bootstrapped environment",
      platform: {
        os: "darwin",
        arch: "arm64",
      },
      serverVersion: "0.0.0-test",
      storageInstanceId: "0d93cbea-f237-4f37-8829-d816667be35f",
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
    } satisfies ExecutionEnvironmentDescriptor;
    writePrimaryEnvironmentDescriptor(descriptor);

    expect(getPrimaryKnownEnvironment()).toEqual({
      id: "environment-local",
      label: "Bootstrapped environment",
      source: "window-origin",
      environmentId: "environment-local",
      storageInstanceId: "0d93cbea-f237-4f37-8829-d816667be35f",
      descriptor,
      target: {
        httpBaseUrl: "http://localhost:3773/",
        wsBaseUrl: "ws://localhost:3773/",
      },
    });
  });

  it("reuses an in-flight descriptor bootstrap request", async () => {
    const testApi = await installDescriptorApi();

    await Promise.all([
      resolveInitialPrimaryEnvironmentDescriptor(),
      resolveInitialPrimaryEnvironmentDescriptor(),
    ]);

    expect(testApi.calls.descriptor).toBe(1);
  });

  it("uses https descriptor urls when the primary environment uses wss", async () => {
    vi.stubEnv("VITE_HTTP_URL", "https://remote.example.com");
    vi.stubEnv("VITE_WS_URL", "wss://remote.example.com");
    await installDescriptorApi();

    await expect(resolveInitialPrimaryEnvironmentDescriptor()).resolves.toEqual(BASE_ENVIRONMENT);
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "https://remote.example.com/.well-known/coddy-studio/environment",
    );
  });

  it("derives the websocket url when only VITE_HTTP_URL is configured", async () => {
    vi.stubEnv("VITE_HTTP_URL", "https://remote.example.com");
    await installDescriptorApi();

    await expect(resolveInitialPrimaryEnvironmentDescriptor()).resolves.toEqual(BASE_ENVIRONMENT);
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "https://remote.example.com/.well-known/coddy-studio/environment",
    );
    expect(getPrimaryKnownEnvironment()?.target).toEqual({
      httpBaseUrl: "https://remote.example.com/",
      wsBaseUrl: "wss://remote.example.com/",
    });
  });

  it("derives the http url when only VITE_WS_URL is configured", async () => {
    vi.stubEnv("VITE_WS_URL", "wss://remote.example.com");
    await installDescriptorApi();

    await expect(resolveInitialPrimaryEnvironmentDescriptor()).resolves.toEqual(BASE_ENVIRONMENT);
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "https://remote.example.com/.well-known/coddy-studio/environment",
    );
    expect(getPrimaryKnownEnvironment()?.target).toEqual({
      httpBaseUrl: "https://remote.example.com/",
      wsBaseUrl: "wss://remote.example.com/",
    });
  });

  it("derives insecure companion protocols when only one local URL is configured", () => {
    vi.stubEnv("VITE_WS_URL", "ws://127.0.0.1:3773");
    expect(readPrimaryEnvironmentTarget().target).toEqual({
      httpBaseUrl: "http://127.0.0.1:3773/",
      wsBaseUrl: "ws://127.0.0.1:3773/",
    });

    vi.unstubAllEnvs();
    vi.stubEnv("VITE_HTTP_URL", "http://127.0.0.1:4773");
    expect(readPrimaryEnvironmentTarget().target).toEqual({
      httpBaseUrl: "http://127.0.0.1:4773/",
      wsBaseUrl: "ws://127.0.0.1:4773/",
    });
  });

  it("routes local development HTTP requests only between distinct loopback origins", () => {
    vi.stubEnv("VITE_HTTP_URL", "http://127.0.0.1:3773");
    vi.stubEnv("VITE_WS_URL", "ws://127.0.0.1:3773");
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://localhost:5733");

    installTestBrowser("http://localhost:5733/");
    expect(resolvePrimaryEnvironmentHttpUrl("/api", { page: "1" })).toBe(
      "http://localhost:5733/api?page=1",
    );

    installTestBrowser("http://localhost:7733/");
    expect(resolvePrimaryEnvironmentHttpUrl("/api")).toBe("http://127.0.0.1:3773/api");

    vi.stubEnv("VITE_DEV_SERVER_URL", "http://example.test:5733");
    installTestBrowser("http://example.test:5733/");
    expect(resolvePrimaryEnvironmentHttpUrl("/api")).toBe("http://127.0.0.1:3773/api");

    vi.stubEnv("VITE_HTTP_URL", "http://api.example.test:3773");
    vi.stubEnv("VITE_WS_URL", "ws://api.example.test:3773");
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://localhost:5733");
    installTestBrowser("http://localhost:5733/");
    expect(resolvePrimaryEnvironmentHttpUrl("/api")).toBe("http://api.example.test:3773/api");
  });

  it("uses the current origin as the descriptor base for local dev environments", async () => {
    installTestBrowser("http://localhost:5735/");
    await installDescriptorApi();

    await expect(resolveInitialPrimaryEnvironmentDescriptor()).resolves.toEqual(BASE_ENVIRONMENT);
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "http://localhost:5735/.well-known/coddy-studio/environment",
    );
  });

  it("uses the managed desktop backend for descriptor requests during local dev", async () => {
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://127.0.0.1:5733");
    vi.stubGlobal("window", {
      location: new URL("http://127.0.0.1:5733/"),
      history: {
        replaceState: vi.fn(),
      },
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            label: "Windows",
            httpBaseUrl: "http://127.0.0.1:3773",
            wsBaseUrl: "ws://127.0.0.1:3773",
            bootstrapToken: "desktop-bootstrap-token",
          },
        ],
      },
    });
    await installDescriptorApi();

    await expect(resolveInitialPrimaryEnvironmentDescriptor()).resolves.toEqual(BASE_ENVIRONMENT);
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "http://127.0.0.1:3773/.well-known/coddy-studio/environment",
    );
  });

  it("retains the URL parser cause without exposing the configured URL in its message", () => {
    vi.stubEnv("VITE_HTTP_URL", "http://[");

    const error = captureThrown(readPrimaryEnvironmentTarget);

    expect(isPrimaryEnvironmentUrlInvalidError(error)).toBe(true);
    if (!isPrimaryEnvironmentUrlInvalidError(error)) {
      throw new Error("Expected a structured primary environment URL error.");
    }
    expect(error).toMatchObject({
      source: "configured",
      urlKind: "http-base-url",
      message: "Could not parse http-base-url for the configured primary environment target.",
    });
    expect(error.cause).toBeInstanceOf(TypeError);
    expect(error.message).not.toContain("http://[");
  });

  it("describes which desktop bootstrap endpoint is missing", () => {
    vi.stubGlobal("window", {
      location: new URL("http://127.0.0.1:5733/"),
      history: { replaceState: vi.fn() },
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            label: "Local environment",
            httpBaseUrl: "http://127.0.0.1:3773",
            bootstrapToken: "desktop-bootstrap-token",
          },
        ],
      },
    });

    const error = captureThrown(readPrimaryEnvironmentTarget);

    expect(isDesktopEnvironmentBootstrapIncompleteError(error)).toBe(true);
    if (!isDesktopEnvironmentBootstrapIncompleteError(error)) {
      throw new Error("Expected a structured desktop bootstrap error.");
    }
    expect(error).toMatchObject({
      hasHttpBaseUrl: true,
      hasWsBaseUrl: false,
      message: "Desktop bootstrap is missing wsBaseUrl for the local environment.",
    });
  });

  it("describes a missing desktop HTTP endpoint", () => {
    vi.stubGlobal("window", {
      location: new URL("http://127.0.0.1:5733/"),
      history: { replaceState: vi.fn() },
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            label: "Local environment",
            wsBaseUrl: "ws://127.0.0.1:3773",
            bootstrapToken: "desktop-bootstrap-token",
          },
        ],
      },
    });

    const error = captureThrown(readPrimaryEnvironmentTarget);
    expect(isDesktopEnvironmentBootstrapIncompleteError(error)).toBe(true);
    if (!isDesktopEnvironmentBootstrapIncompleteError(error)) {
      throw new Error("Expected a structured desktop bootstrap error.");
    }
    expect(error).toMatchObject({
      hasHttpBaseUrl: false,
      hasWsBaseUrl: true,
      message: "Desktop bootstrap is missing httpBaseUrl for the local environment.",
    });
  });

  it.each([
    { runtime: "Windows packaged", url: "http://tauri.localhost/", configuredUrl: "" },
    { runtime: "Linux/macOS packaged", url: "tauri://localhost/", configuredUrl: "" },
    {
      runtime: "desktop development",
      url: "http://127.0.0.1:5733/",
      configuredUrl: "http://127.0.0.1:13773/",
    },
  ])("withholds the primary target without a bootstrap in $runtime", ({ url, configuredUrl }) => {
    vi.stubEnv("VITE_HTTP_URL", configuredUrl);
    vi.stubEnv("VITE_WS_URL", "");
    vi.stubGlobal("window", {
      location: new URL(url),
      history: { replaceState: vi.fn() },
      desktopBridge: { getLocalEnvironmentBootstraps: () => [] },
    });

    expect(readPrimaryEnvironmentTarget).toThrowError(
      expect.objectContaining({ _tag: "DesktopPrimaryEnvironmentWithheldError" }),
    );
    expect(() => resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toThrowError(
      expect.objectContaining({ _tag: "DesktopPrimaryEnvironmentWithheldError" }),
    );
    expect(
      isDesktopPrimaryEnvironmentWithheldError(captureThrown(readPrimaryEnvironmentTarget)),
    ).toBe(true);
  });

  it("withholds the primary target when only secondary desktop bootstraps exist", () => {
    vi.stubGlobal("window", {
      location: new URL("http://tauri.localhost/"),
      history: { replaceState: vi.fn() },
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "wsl:ubuntu",
            label: "Ubuntu",
            httpBaseUrl: "http://127.0.0.1:4773/",
            wsBaseUrl: "ws://127.0.0.1:4773/",
          },
        ],
      },
    });

    expect(readPrimaryEnvironmentTarget).toThrowError(
      expect.objectContaining({ _tag: "DesktopPrimaryEnvironmentWithheldError" }),
    );
  });

  it("withholds empty desktop endpoint entries", () => {
    vi.stubGlobal("window", {
      location: new URL("https://app.example.test/"),
      history: { replaceState: vi.fn() },
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            label: "Local environment",
            bootstrapToken: "desktop-bootstrap-token",
          },
        ],
      },
    });

    expect(readPrimaryEnvironmentTarget).toThrowError(
      expect.objectContaining({ _tag: "DesktopPrimaryEnvironmentWithheldError" }),
    );
  });

  it("supports an HTTPS window-origin fallback in browser mode", () => {
    installTestBrowser("https://app.example.test/");

    expect(readPrimaryEnvironmentTarget()).toEqual({
      source: "window-origin",
      target: {
        httpBaseUrl: "https://app.example.test/",
        wsBaseUrl: "wss://app.example.test/",
      },
    });
    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/coddy-studio/environment")).toBe(
      "https://app.example.test/.well-known/coddy-studio/environment",
    );
  });

  it("retains a withheld desktop primary registration and reuses it when its bootstrap returns", () => {
    let bootstraps: DesktopEnvironmentBootstrap[] = [];
    vi.stubGlobal("window", {
      location: new URL("http://tauri.localhost/"),
      history: { replaceState: vi.fn() },
      desktopBridge: { getLocalEnvironmentBootstraps: () => bootstraps },
    });
    const registration = new PrimaryConnectionRegistration({
      target: new PrimaryConnectionTarget({
        environmentId: EnvironmentId.make("primary"),
        label: "Local",
        httpBaseUrl: "http://127.0.0.1:3773/",
        wsBaseUrl: "ws://127.0.0.1:3773/",
      }),
    });
    const cached = {
      signature: "primary|http://127.0.0.1:3773/|ws://127.0.0.1:3773/",
      registration,
    };
    const previous = new Map([["primary", cached]]);

    const withheld = readPrimaryEnvironmentTargetResult();
    expect(withheld).toMatchObject({
      _tag: "Failure",
      cause: { _tag: "DesktopPrimaryEnvironmentWithheldError" },
    });
    const retained = primaryRegistrationToRetainAfterTopologyRead(previous, withheld);
    expect(retained).toBe(cached);
    if (retained === undefined) {
      throw new Error("Expected the cached primary registration to be retained.");
    }

    bootstraps = [
      {
        id: "primary",
        label: "Local",
        httpBaseUrl: "http://127.0.0.1:3773",
        wsBaseUrl: "ws://127.0.0.1:3773",
      },
    ];
    const restored = readPrimaryEnvironmentTargetResult();
    expect(restored).toEqual({
      _tag: "Success",
      target: {
        source: "desktop-managed",
        target: {
          httpBaseUrl: "http://127.0.0.1:3773/",
          wsBaseUrl: "ws://127.0.0.1:3773/",
        },
      },
    });
    if (restored._tag !== "Success" || restored.target === null) {
      throw new Error("Expected the desktop primary bootstrap to be restored.");
    }
    const signature = `primary|${restored.target.target.httpBaseUrl}|${restored.target.target.wsBaseUrl}`;
    expect(canReuseCachedPlatformRegistration(retained, signature, 6_000)).toBe(true);
    expect(retained.registration).toBe(registration);
  });

  it("preserves an unsupported window-origin protocol", () => {
    vi.stubGlobal("window", {
      location: { origin: "file:///tmp/coddy-studio/" },
      history: { replaceState: vi.fn() },
    });

    const error = captureThrown(readPrimaryEnvironmentTarget);

    expect(isPrimaryEnvironmentProtocolUnsupportedError(error)).toBe(true);
    if (!isPrimaryEnvironmentProtocolUnsupportedError(error)) {
      throw new Error("Expected a structured primary environment protocol error.");
    }
    expect(error).toMatchObject({
      source: "window-origin",
      protocol: "file:",
      message: "The window-origin primary environment target uses unsupported protocol file:.",
    });
  });
});

import { it as effectIt } from "@effect/vitest";
import { IDBFactory } from "fake-indexeddb";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import { PlatformConnectionSource } from "@coddy/client-runtime/platform";
import { connectionPlatformLayer } from "../../connection/platform";
import { PrimaryEnvironmentHttpClient, layer as primaryHttpClientLayer } from "./httpClient";
import { makePrimaryEnvironmentHttpLayer } from "./httpLayer";

describe("cookie primary platform bootstrap", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  effectIt.effect(
    "uses the same proxy HTTP target after cookie auth and during platform discovery",
    () => {
      const ui = "http://127.0.0.1:4885";
      vi.stubEnv("VITE_HTTP_URL", "http://127.0.0.1:4887");
      vi.stubEnv("VITE_WS_URL", "ws://127.0.0.1:4887");
      vi.stubEnv("VITE_DEV_SERVER_URL", ui);
      installTestBrowser(ui + "/");
      vi.stubGlobal("indexedDB", new IDBFactory());
      let cookieStored = false;
      const requests: Array<{ path: string; port: string; credentials: RequestCredentials }> = [];
      vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
        const request = new Request(input, init),
          url = new URL(request.url);
        requests.push({ path: url.pathname, port: url.port, credentials: request.credentials });
        // Node has no browser cookie jar or CORS enforcement. Model the normal Rust
        // wildcard/no-credentials CORS response, with actual URLs/options from the client.
        if (url.origin !== ui && request.credentials === "include")
          throw new TypeError("Credentialed cross-origin wildcard response refused");
        const permitsCookie =
          request.credentials !== "omit" &&
          (request.credentials === "include" || url.origin === ui);
        if (url.pathname === "/api/auth/browser-session") {
          cookieStored = permitsCookie;
          return Response.json({
            authenticated: true,
            scopes: ["orchestration:read"],
            sessionMethod: "browser-session-cookie",
            expiresAt: "2030-10-06T00:00:00.000Z",
          });
        }
        if (url.pathname === "/api/auth/session")
          return Response.json({
            authenticated: cookieStored && permitsCookie,
            auth: {
              policy: "loopback-browser",
              bootstrapMethods: ["one-time-token"],
              sessionMethods: ["browser-session-cookie", "bearer-access-token"],
              sessionCookieName: "inert-session",
            },
          });
        if (url.pathname === "/.well-known/coddy-studio/environment")
          return Response.json(BASE_ENVIRONMENT);
        throw new Error("Unexpected primary bootstrap request");
      });
      return Effect.gen(function* () {
        const session = yield* Effect.gen(function* () {
          const client = yield* PrimaryEnvironmentHttpClient;
          yield* client.auth.browserSession({
            payload: { credential: "inert-pairing-credential" },
          });
          return yield* client.auth.session({ headers: {} });
        }).pipe(
          Effect.provide(
            primaryHttpClientLayer.pipe(Layer.provide(makePrimaryEnvironmentHttpLayer())),
          ),
        );
        expect(session.authenticated).toBe(true);
        const source = yield* PlatformConnectionSource;
        const registrations = Option.getOrThrow(yield* Stream.runHead(source.registrations));
        expect(registrations).toHaveLength(1);
        expect(registrations[0]?.target).toMatchObject({
          environmentId: BASE_ENVIRONMENT.environmentId,
          httpBaseUrl: ui,
          wsBaseUrl: "ws://127.0.0.1:4887/",
        });
        expect(requests.map((request) => [request.path, request.port])).toEqual([
          ["/api/auth/browser-session", "4885"],
          ["/api/auth/session", "4885"],
          ["/.well-known/coddy-studio/environment", "4885"],
        ]);
        expect(requests.at(-1)?.credentials).toBe("include");
      }).pipe(Effect.provide(connectionPlatformLayer), Effect.scoped);
    },
  );
});
