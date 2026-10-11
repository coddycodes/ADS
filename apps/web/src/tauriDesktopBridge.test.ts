import {
  BearerConnectionTarget,
  ConnectionTransientError,
  decideStorageIdentity,
  type PreparedConnection,
  verifyPreparedStorageIdentity,
} from "@bibcode/client-runtime/connection";
import { AcceptedStorageIdentityStore } from "@bibcode/client-runtime/platform";
import { makeEnvironmentHttpApiClient, remoteHttpClientLayer } from "@bibcode/client-runtime/rpc";
import { DEFAULT_CLIENT_SETTINGS, type DesktopBridge, EnvironmentId } from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { it as effectIt } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { IDBFactory } from "fake-indexeddb";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { connectionStorageLayer } from "./connection/storage";

type TauriEventHandler = (event: { payload: unknown }) => void;

const { showContextMenuFallbackMock, startBrowserSurfaceSyncMock } = vi.hoisted(() => ({
  showContextMenuFallbackMock: vi.fn(),
  startBrowserSurfaceSyncMock: vi.fn(),
}));

vi.mock("./browser/browserSurfaceSync", () => ({
  startBrowserSurfaceSync: startBrowserSurfaceSyncMock,
}));

vi.mock("./contextMenuFallback", () => ({
  showContextMenuFallback: showContextMenuFallbackMock,
}));

const unsupportedSshError = {
  code: "tauri_capability_unsupported",
  method: "ensureSshEnvironment",
  capability: "sshProvisioning",
  message: "ensureSshEnvironment requires sshProvisioning, which is temporarily unavailable.",
};

const unsupportedContextMenuError = {
  code: "tauri_capability_unsupported",
  method: "showContextMenu",
  capability: "nativeContextMenu",
  message: "showContextMenu requires nativeContextMenu, which is temporarily unavailable.",
};

const defaultLocalEnvironmentBootstrap = {
  id: "primary",
  label: "Local",
  httpBaseUrl: "http://127.0.0.1:3773",
  wsBaseUrl: "ws://127.0.0.1:3773",
  bootstrapToken: "bootstrap-token",
};

function installTauriHarness(options?: {
  readonly previewSupported?: boolean;
  readonly protectedConnectionCatalog?: boolean;
  readonly rejectMetadata?: boolean;
  readonly metadataError?: unknown;
  readonly bridgeVersion?: number;
  readonly omitProtectedConnectionCatalog?: boolean;
  readonly contextMenuResult?: string | null;
  readonly rejectContextMenu?: unknown;
  readonly rejectSshProvisioning?: boolean;
  readonly localEnvironmentBootstraps?: readonly unknown[] | (() => readonly unknown[]);
  readonly nativeConnectionCatalog?: string | null | (() => string | null);
  readonly compareAndSetConnectionCatalog?: (
    expectedCatalog: string | null,
    nextCatalog: string,
  ) => boolean;
  readonly compareConnectionCatalog?: (expectedCatalog: string | null) => boolean;
  readonly rejectConnectionCatalogCompareAndSet?: unknown;
  readonly clearConnectionCatalog?: () => void;
  readonly rejectFallbackCommands?: boolean;
  readonly rejectListeners?: boolean;
}) {
  const listeners = new Map<string, TauriEventHandler>();
  const unlisteners = new Map<string, ReturnType<typeof vi.fn>>();

  const invoke = vi.fn((command: string, args?: Record<string, unknown>) => {
    switch (command) {
      case "desktop_bridge_get_bridge_metadata":
        if (options?.rejectMetadata) {
          return Promise.reject(options.metadataError ?? new Error("bridge metadata unavailable"));
        }
        return Promise.resolve({
          host: "tauri",
          bridgeVersion: options?.bridgeVersion ?? 3,
          features: {
            localBackend: true,
            localBearerToken: true,
            clientSettings: true,
            serverExposure: true,
            wslDiscovery: true,
            sshRemoteHttp: true,
            connectionCatalog: true,
            ...(options?.omitProtectedConnectionCatalog
              ? {}
              : {
                  protectedConnectionCatalog: options?.protectedConnectionCatalog ?? true,
                }),
            preview: options?.previewSupported ?? false,
            updater: false,
            menuEvents: true,
            sshProvisioning: true,
          },
        });
      case "desktop_bridge_check_for_update":
        return Promise.resolve({
          checked: false,
          state: { status: "disabled" },
        });
      case "desktop_bridge_download_update":
        return Promise.resolve({
          accepted: false,
          completed: false,
          state: { status: "disabled" },
        });
      case "desktop_bridge_install_update":
        return Promise.resolve({
          accepted: false,
          completed: false,
          state: { status: "disabled" },
        });
      case "desktop_bridge_save_diagnostic_logs":
        return Promise.resolve("C:\\Users\\test\\Downloads\\diagnostics.zip");
      case "desktop_bridge_pick_files":
        return Promise.resolve(["C:\\workspace\\demo\\a.txt", "C:\\workspace\\demo\\b.txt"]);
      case "desktop_bridge_download_to_folder":
        return Promise.resolve("C:\\Users\\test\\Downloads\\src.zip");
      case "desktop_bridge_upload_file":
        return Promise.resolve({ status: 201, body: "{}" });
      case "desktop_bridge_ensure_ssh_environment":
        if (options?.rejectSshProvisioning) {
          return Promise.reject(unsupportedSshError);
        }
        return Promise.resolve({
          target: args?.target,
          httpBaseUrl: "http://127.0.0.1:3773/",
          wsBaseUrl: "ws://127.0.0.1:3773/",
          pairingToken: "ssh-pairing-token",
          remotePort: 3773,
          remoteServerKind: "managed",
        });
      case "desktop_bridge_ssh_forward":
        return Promise.resolve(45123);
      case "desktop_bridge_show_context_menu":
        if ("rejectContextMenu" in (options ?? {})) {
          return Promise.reject(options?.rejectContextMenu);
        }
        return Promise.resolve(options?.contextMenuResult ?? null);
      case "desktop_bridge_get_local_environment_bootstraps":
        return Promise.resolve(
          typeof options?.localEnvironmentBootstraps === "function"
            ? options.localEnvironmentBootstraps()
            : (options?.localEnvironmentBootstraps ?? [defaultLocalEnvironmentBootstrap]),
        );
      case "desktop_bridge_get_connection_catalog":
        return options?.rejectFallbackCommands
          ? Promise.reject(new Error("native catalog unavailable"))
          : Promise.resolve(
              options !== undefined && "nativeConnectionCatalog" in options
                ? typeof options.nativeConnectionCatalog === "function"
                  ? options.nativeConnectionCatalog()
                  : options.nativeConnectionCatalog
                : "native-catalog",
            );
      case "desktop_bridge_set_connection_catalog":
        return options?.rejectFallbackCommands
          ? Promise.reject(new Error("native catalog unavailable"))
          : Promise.resolve(args?.catalog === "saved-catalog");
      case "desktop_bridge_compare_and_set_connection_catalog":
        if ("rejectConnectionCatalogCompareAndSet" in (options ?? {})) {
          return Promise.reject(options?.rejectConnectionCatalogCompareAndSet);
        }
        return options?.rejectFallbackCommands
          ? Promise.reject(new Error("native catalog unavailable"))
          : Promise.resolve(
              options?.compareAndSetConnectionCatalog?.(
                (args?.expectedCatalog as string | null) ?? null,
                args?.nextCatalog as string,
              ) ??
                (args?.expectedCatalog === "native-catalog" &&
                  args?.nextCatalog === "saved-catalog"),
            );
      case "desktop_bridge_compare_connection_catalog":
        return options?.rejectFallbackCommands
          ? Promise.reject(new Error("native catalog unavailable"))
          : Promise.resolve(
              options?.compareConnectionCatalog?.(
                (args?.expectedCatalog as string | null) ?? null,
              ) ?? args?.expectedCatalog === "native-catalog",
            );
      case "desktop_bridge_clear_connection_catalog":
        if (options?.rejectFallbackCommands) {
          return Promise.reject(new Error("native catalog unavailable"));
        }
        options?.clearConnectionCatalog?.();
        return Promise.resolve(undefined);
      case "desktop_bridge_fetch_environment_descriptor":
        return Promise.resolve({ environmentId: "ssh-env" });
      case "desktop_bridge_bootstrap_ssh_bearer_session":
        return Promise.resolve({ access_token: "ssh-bearer" });
      case "desktop_bridge_fetch_ssh_session_state":
        return Promise.resolve({ authenticated: true });
      case "desktop_bridge_issue_ssh_web_socket_ticket":
        return Promise.resolve({ ticket: "ws-ticket" });
      default:
        return options?.rejectFallbackCommands
          ? Promise.reject(new Error(`unsupported fallback command: ${command}`))
          : Promise.resolve(null);
    }
  });

  const listen = vi.fn(async (event: string, handler: TauriEventHandler) => {
    if (options?.rejectListeners) {
      throw new Error(`listener unavailable: ${event}`);
    }
    listeners.set(event, handler);
    const unlisten = vi.fn(() => {
      listeners.delete(event);
    });
    unlisteners.set(event, unlisten);
    return unlisten;
  });

  vi.stubGlobal("window", {
    __TAURI__: {
      core: { invoke },
      event: { listen },
    },
    desktopBridge: undefined,
  });

  return { invoke, listen, listeners, unlisteners };
}

async function installBridge(): Promise<DesktopBridge> {
  const { tauriDesktopBridgeReady } = await import("./tauriDesktopBridge");
  await tauriDesktopBridgeReady;
  const bridge = window.desktopBridge;
  if (!bridge) {
    throw new Error("Expected Tauri adapter to install window.desktopBridge.");
  }
  return bridge;
}

const SENSITIVE_CONNECTION_CATALOG = JSON.stringify({
  schemaVersion: 1,
  targets: [
    {
      _tag: "BearerConnectionTarget",
      environmentId: "sensitive-environment",
      label: "Sensitive remote",
      connectionId: "sensitive-connection",
    },
  ],
  profiles: [
    {
      _tag: "BearerConnectionProfile",
      connectionId: "sensitive-connection",
      environmentId: "sensitive-environment",
      label: "Sensitive remote",
      httpBaseUrl: "https://private.example.test/secret-path",
      wsBaseUrl: "wss://private.example.test/secret-path",
    },
  ],
  credentials: [
    {
      connectionId: "sensitive-connection",
      credential: {
        _tag: "BearerConnectionCredential",
        token: "secret-bearer-token",
      },
    },
  ],
  remoteDpopTokens: [
    {
      environmentId: "sensitive-environment",
      label: "Sensitive remote",
      endpoint: {
        httpBaseUrl: "https://private.example.test/secret-path",
        wsBaseUrl: "wss://private.example.test/secret-path",
        providerKind: "cloudflare_tunnel",
      },
      accessToken: "secret-dpop-token",
      expiresAtEpochMs: 1_000_000,
      dpopThumbprint: "secret-thumbprint",
    },
  ],
  acceptedStorageIdentities: [
    {
      targetKey: "bearer:sensitive-connection",
      storageInstanceId: "sensitive-store-id",
    },
  ],
});

function sensitivePrepared(storageInstanceId: string | null): PreparedConnection {
  const environmentId = EnvironmentId.make("sensitive-environment");
  const target = new BearerConnectionTarget({
    environmentId,
    label: "Sensitive remote",
    connectionId: "sensitive-connection",
    serverEnvironmentId: null,
  });
  return {
    environmentId,
    label: target.label,
    descriptor: {
      environmentId,
      label: target.label,
      platform: { os: "windows", arch: "x64" },
      serverVersion: "0.0.0-test",
      storageInstanceId,
      bootId: null,
      remoteUpdateSupport: null,
      remoteProtocolVersion: 1,
      minCompatibleRemoteProtocol: 1,
      capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
    },
    httpBaseUrl: "https://private.example.test/secret-path",
    socketUrl: "wss://private.example.test/secret-path/ws",
    httpAuthorization: null,
    e2ee: null,
    target,
  };
}

function readIndexedDbConnectionCatalog(factory: IDBFactory): Promise<string | null> {
  return new Promise((resolve, reject) => {
    const open = factory.open("bibcode:connection-runtime", 2);
    open.addEventListener("error", () => reject(open.error));
    open.addEventListener("success", () => {
      const database = open.result;
      const request = database
        .transaction("catalog", "readonly")
        .objectStore("catalog")
        .get("document");
      request.addEventListener("error", () => reject(request.error));
      request.addEventListener("success", () => {
        const raw = request.result;
        database.close();
        resolve(typeof raw === "string" ? raw : null);
      });
    });
  });
}

describe("tauriDesktopBridge", () => {
  afterEach(() => {
    vi.resetModules();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    showContextMenuFallbackMock.mockReset();
    startBrowserSurfaceSyncMock.mockReset();
  });

  it("starts browser surface sync once when installing the Tauri preview bridge", async () => {
    installTauriHarness({ previewSupported: true });

    const bridge = await installBridge();
    await import("./tauriDesktopBridge");

    expect(startBrowserSurfaceSyncMock).toHaveBeenCalledTimes(1);
    expect(startBrowserSurfaceSyncMock).toHaveBeenCalledWith(bridge.preview);
  });

  it("publishes supported preview before readiness-gated consumers cache it", async () => {
    installTauriHarness({ previewSupported: true });

    const bridge = await installBridge();
    const { previewBridge } = await import("./components/preview/previewBridge");

    expect(previewBridge).toBe(bridge.preview);
  });

  it("omits the preview bridge when the native host reports it unsupported", async () => {
    installTauriHarness({ previewSupported: false });

    const bridge = await installBridge();

    expect(bridge.preview).toBeUndefined();
    expect(startBrowserSurfaceSyncMock).not.toHaveBeenCalled();
  });

  it("fails closed when native preview support cannot be determined", async () => {
    installTauriHarness({ rejectMetadata: true });

    const bridge = await installBridge();

    expect(bridge.preview).toBeUndefined();
    expect(startBrowserSurfaceSyncMock).not.toHaveBeenCalled();
  });

  it("does not start browser surface sync in a browser runtime", async () => {
    vi.stubGlobal("window", { desktopBridge: undefined });

    const { tauriDesktopBridgeReady } = await import("./tauriDesktopBridge");
    await tauriDesktopBridgeReady;

    expect(window.desktopBridge).toBeUndefined();
    expect(startBrowserSurfaceSyncMock).not.toHaveBeenCalled();
  });

  it("does not start browser surface sync when a desktop bridge already exists", async () => {
    installTauriHarness();
    const existingBridge = { preview: { setBounds: vi.fn() } } as unknown as DesktopBridge;
    window.desktopBridge = existingBridge;

    const { tauriDesktopBridgeReady } = await import("./tauriDesktopBridge");
    await tauriDesktopBridgeReady;

    expect(window.desktopBridge).toBe(existingBridge);
    expect(startBrowserSurfaceSyncMock).not.toHaveBeenCalled();
  });

  it("waits for the primary bootstrap before reporting the Tauri bridge ready", async () => {
    let reads = 0;
    const primaryBootstrap = {
      id: "primary",
      label: "Local",
      httpBaseUrl: "http://127.0.0.1:3773",
      wsBaseUrl: "ws://127.0.0.1:3773",
      bootstrapToken: "bootstrap-token",
    };
    installTauriHarness({
      localEnvironmentBootstraps: () => (++reads === 1 ? [] : [primaryBootstrap]),
    });

    const { tauriDesktopBridgeReady } = await import("./tauriDesktopBridge");
    await tauriDesktopBridgeReady;

    expect(reads).toBeGreaterThanOrEqual(2);
    expect(window.desktopBridge?.getLocalEnvironmentBootstraps()).toEqual([primaryBootstrap]);
  });

  it("routes SSH remote API helpers through Tauri commands", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(bridge.fetchSshEnvironmentDescriptor("http://127.0.0.1:3773")).resolves.toEqual({
      environmentId: "ssh-env",
    });
    await expect(
      bridge.bootstrapSshBearerSession("http://127.0.0.1:3773", "pairing-token"),
    ).resolves.toEqual({ access_token: "ssh-bearer" });
    await expect(
      bridge.bootstrapSshBearerSession("http://127.0.0.1:3773", "scoped-token", [
        "orchestration:read",
        "terminal:operate",
      ]),
    ).resolves.toEqual({ access_token: "ssh-bearer" });
    await expect(
      bridge.fetchSshSessionState("http://127.0.0.1:3773", "bearer-token"),
    ).resolves.toEqual({ authenticated: true });
    await expect(
      bridge.issueSshWebSocketTicket("http://127.0.0.1:3773", "bearer-token"),
    ).resolves.toEqual({ ticket: "ws-ticket" });

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_fetch_environment_descriptor", {
      httpBaseUrl: "http://127.0.0.1:3773",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_bootstrap_ssh_bearer_session", {
      httpBaseUrl: "http://127.0.0.1:3773",
      credential: "pairing-token",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_bootstrap_ssh_bearer_session", {
      httpBaseUrl: "http://127.0.0.1:3773",
      credential: "scoped-token",
      scopes: ["orchestration:read", "terminal:operate"],
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_fetch_ssh_session_state", {
      httpBaseUrl: "http://127.0.0.1:3773",
      bearerToken: "bearer-token",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_issue_ssh_web_socket_ticket", {
      httpBaseUrl: "http://127.0.0.1:3773",
      bearerToken: "bearer-token",
    });
  });

  it("exposes Tauri bridge metadata and structured unsupported errors", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(bridge.getHostMetadata?.()).resolves.toEqual({
      host: "tauri",
      bridgeVersion: 3,
      features: {
        localBackend: true,
        localBearerToken: true,
        clientSettings: true,
        serverExposure: true,
        wslDiscovery: true,
        sshRemoteHttp: true,
        connectionCatalog: true,
        protectedConnectionCatalog: true,
        preview: false,
        updater: false,
        menuEvents: true,
        sshProvisioning: true,
      },
    });
    const branding = bridge.getAppBranding();
    expect(branding?.baseName).toBe("BiBCode");
    expect(["Latest", "Dev", "Nightly"]).toContain(branding?.stageLabel);
    expect(branding?.displayName).toBe(
      branding?.stageLabel === "Latest" ? "BiBCode" : `BiBCode (${branding?.stageLabel})`,
    );

    await expect(
      bridge.ensureSshEnvironment({
        alias: "host-1",
        hostname: "example.test",
        username: null,
        port: null,
      }),
    ).resolves.toEqual({
      target: {
        alias: "host-1",
        hostname: "example.test",
        username: null,
        port: null,
      },
      httpBaseUrl: "http://127.0.0.1:3773/",
      wsBaseUrl: "ws://127.0.0.1:3773/",
      pairingToken: "ssh-pairing-token",
      remotePort: 3773,
      remoteServerKind: "managed",
    });

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_get_bridge_metadata", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_ensure_ssh_environment", {
      target: {
        alias: "host-1",
        hostname: "example.test",
        username: null,
        port: null,
      },
      options: undefined,
    });
  });

  it("routes connection catalog persistence through Tauri commands", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    expect(bridge.getConnectionCatalog).toBeDefined();
    expect(bridge.setConnectionCatalog).toBeDefined();
    expect(bridge.compareAndSetConnectionCatalog).toBeDefined();
    expect(bridge.compareConnectionCatalog).toBeDefined();
    expect(bridge.clearConnectionCatalog).toBeDefined();

    await expect(bridge.getConnectionCatalog!()).resolves.toBe("native-catalog");
    await expect(bridge.setConnectionCatalog!("saved-catalog")).resolves.toBe(true);
    await expect(
      bridge.compareAndSetConnectionCatalog!("native-catalog", "saved-catalog"),
    ).resolves.toBe(true);
    await expect(bridge.compareConnectionCatalog!("native-catalog")).resolves.toBe(true);
    await expect(bridge.clearConnectionCatalog!()).resolves.toBeUndefined();

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_get_connection_catalog", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_set_connection_catalog", {
      catalog: "saved-catalog",
    });
    expect(harness.invoke).toHaveBeenCalledWith(
      "desktop_bridge_compare_and_set_connection_catalog",
      {
        expectedCatalog: "native-catalog",
        nextCatalog: "saved-catalog",
      },
    );
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_compare_connection_catalog", {
      expectedCatalog: "native-catalog",
    });
    expect(harness.invoke).toHaveBeenCalledWith(
      "desktop_bridge_clear_connection_catalog",
      undefined,
    );
  });

  it("collapses a protected legacy catalog into native storage before publishing the bridge", async () => {
    const storage = new Map<string, string>([
      ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
    ]);
    const localStorage = {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
    };
    let nativeCatalog: string | null = null;
    const compareAndSetConnectionCatalog = vi.fn(
      (expectedCatalog: string | null, nextCatalog: string) => {
        if (nativeCatalog !== expectedCatalog) return false;
        nativeCatalog = nextCatalog;
        return true;
      },
    );
    const harness = installTauriHarness({
      nativeConnectionCatalog: () => nativeCatalog,
      compareAndSetConnectionCatalog,
      compareConnectionCatalog: (expected) => nativeCatalog === expected,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);
    const bridge = await installBridge();

    expect(nativeCatalog).toBe(SENSITIVE_CONNECTION_CATALOG);
    expect(storage.has("bibcode.connectionCatalog")).toBe(false);
    await expect(bridge.getConnectionCatalog!()).resolves.toBe(SENSITIVE_CONNECTION_CATALOG);
    await expect(bridge.compareConnectionCatalog!(SENSITIVE_CONNECTION_CATALOG)).resolves.toBe(
      true,
    );
    expect(compareAndSetConnectionCatalog).toHaveBeenCalledTimes(1);
    expect(compareAndSetConnectionCatalog).toHaveBeenCalledWith(null, SENSITIVE_CONNECTION_CATALOG);
    expect(harness.invoke).toHaveBeenCalledWith(
      "desktop_bridge_compare_and_set_connection_catalog",
      { expectedCatalog: null, nextCatalog: SENSITIVE_CONNECTION_CATALOG },
    );
  });

  effectIt.effect(
    "keeps accepted changed and unverifiable decisions read-only after protected migration",
    () =>
      Effect.gen(function* () {
        vi.stubGlobal("indexedDB", new IDBFactory());
        const storage = new Map<string, string>([
          ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
        ]);
        const localStorage = {
          getItem: vi.fn((key: string) => storage.get(key) ?? null),
          setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
          removeItem: vi.fn((key: string) => storage.delete(key)),
        };
        let nativeCatalog: string | null = null;
        const compareAndSetConnectionCatalog = vi.fn(
          (expectedCatalog: string | null, nextCatalog: string) => {
            if (nativeCatalog !== expectedCatalog) return false;
            nativeCatalog = nextCatalog;
            return true;
          },
        );
        installTauriHarness({
          nativeConnectionCatalog: () => nativeCatalog,
          compareAndSetConnectionCatalog,
          compareConnectionCatalog: (expected) => nativeCatalog === expected,
        });
        Object.assign(window, { localStorage });
        vi.stubGlobal("localStorage", localStorage);
        yield* Effect.promise(() => installBridge());

        const decisions = yield* Effect.gen(function* () {
          const identities = yield* AcceptedStorageIdentityStore;
          const transition = (reported: string | null) =>
            identities.transition("bearer:sensitive-connection", (accepted) => ({
              result: decideStorageIdentity(accepted, reported),
              mutation: { _tag: "Keep" as const },
            }));
          const accepted = yield* transition("sensitive-store-id");
          const unverifiable = yield* transition(null);
          const changed = yield* verifyPreparedStorageIdentity(
            sensitivePrepared("different-store-id"),
          ).pipe(Effect.result);
          return { accepted, unverifiable, changed };
        }).pipe(Effect.provide(connectionStorageLayer));

        expect(decisions.accepted).toEqual({ _tag: "Accepted", value: "sensitive-store-id" });
        expect(decisions.unverifiable).toEqual({
          _tag: "Unverifiable",
          accepted: "sensitive-store-id",
        });
        expect(decisions.changed).toMatchObject({
          _tag: "Failure",
          failure: {
            _tag: "ConnectionStorageChangedError",
            acceptedStorageInstanceId: "sensitive-store-id",
            reportedStorageInstanceId: "different-store-id",
          },
        });
        expect(compareAndSetConnectionCatalog).toHaveBeenCalledTimes(1);
        expect(storage.has("bibcode.connectionCatalog")).toBe(false);
      }),
  );

  it("preserves a concurrent native winner while collapsing a protected legacy catalog", async () => {
    const storage = new Map<string, string>([
      ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
    ]);
    const localStorage = {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
    };
    const nativeWinner = "native-concurrent-winner";
    let nativeCatalog: string | null = null;
    const compareAndSetConnectionCatalog = vi.fn(() => {
      nativeCatalog = nativeWinner;
      return false;
    });
    installTauriHarness({
      nativeConnectionCatalog: () => nativeCatalog,
      compareAndSetConnectionCatalog,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);

    const bridge = await installBridge();

    expect(nativeCatalog).toBe(nativeWinner);
    await expect(bridge.getConnectionCatalog!()).resolves.toBe(nativeWinner);
    expect(compareAndSetConnectionCatalog).toHaveBeenCalledOnce();
    expect(storage.has("bibcode.connectionCatalog")).toBe(false);
  });

  it("keeps an existing native catalog authoritative over a stale renderer copy", async () => {
    const storage = new Map<string, string>([
      ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
    ]);
    const localStorage = {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
    };
    const compareAndSetConnectionCatalog = vi.fn(() => false);
    const harness = installTauriHarness({
      nativeConnectionCatalog: "native-existing-winner",
      compareAndSetConnectionCatalog,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);

    const bridge = await installBridge();

    await expect(bridge.getConnectionCatalog!()).resolves.toBe("native-existing-winner");
    expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    expect(storage.has("bibcode.connectionCatalog")).toBe(false);
    const nativeRead = harness.invoke.mock.invocationCallOrder.find(
      (_, index) =>
        harness.invoke.mock.calls[index]?.[0] === "desktop_bridge_get_connection_catalog",
    );
    expect(nativeRead).toBeLessThan(localStorage.removeItem.mock.invocationCallOrder[0]!);
  });

  effectIt.effect("fails protected catalog migration closed without changing either source", () =>
    Effect.gen(function* () {
      const indexedDb = new IDBFactory();
      vi.stubGlobal("indexedDB", indexedDb);
      const storage = new Map<string, string>([
        ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
      ]);
      const localStorage = {
        getItem: vi.fn((key: string) => storage.get(key) ?? null),
        setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
        removeItem: vi.fn((key: string) => storage.delete(key)),
      };
      let nativeCatalog: string | null = null;
      installTauriHarness({
        nativeConnectionCatalog: () => nativeCatalog,
        rejectConnectionCatalogCompareAndSet: new Error(
          "DPAPI failed for C:\\Users\\alice with secret-bearer-token",
        ),
      });
      Object.assign(window, { localStorage });
      vi.stubGlobal("localStorage", localStorage);

      const bridge = yield* Effect.promise(() => installBridge());
      const bridgeResult = yield* Effect.promise(() =>
        bridge.getConnectionCatalog!().then(
          () => "resolved",
          (error: unknown) => String(error),
        ),
      );
      const persistenceResult = yield* Effect.gen(function* () {
        const identities = yield* AcceptedStorageIdentityStore;
        yield* identities.accept({
          targetKey: "platform:primary",
          storageInstanceId: "must-not-be-persisted",
        });
      }).pipe(Effect.provide(connectionStorageLayer), Effect.result);

      expect(bridgeResult).toContain("protection capability could not be verified");
      expect(bridgeResult).not.toContain("secret-bearer-token");
      expect(bridgeResult).not.toContain("C:\\Users\\alice");
      expect(persistenceResult._tag).toBe("Failure");
      if (persistenceResult._tag === "Failure") {
        expect(persistenceResult.failure).toBeInstanceOf(ConnectionTransientError);
        expect(String(persistenceResult.failure)).not.toContain("secret-bearer-token");
        expect(String(persistenceResult.failure)).not.toContain("C:\\Users\\alice");
      }
      expect(nativeCatalog).toBeNull();
      expect(storage.get("bibcode.connectionCatalog")).toBe(SENSITIVE_CONNECTION_CATALOG);
      expect(localStorage.removeItem).not.toHaveBeenCalled();
      expect(yield* Effect.promise(() => readIndexedDbConnectionCatalog(indexedDb))).toBeNull();
    }),
  );

  it("fails protected catalog initialization closed when the renderer legacy source is unreadable", async () => {
    const localStorage = {
      getItem: vi.fn(() => {
        throw new Error("renderer storage failed with secret-bearer-token");
      }),
      setItem: vi.fn(),
      removeItem: vi.fn(),
    };
    const compareAndSetConnectionCatalog = vi.fn(() => true);
    installTauriHarness({
      nativeConnectionCatalog: null,
      compareAndSetConnectionCatalog,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);

    const bridge = await installBridge();
    const result = await bridge.getConnectionCatalog!().then(
      () => "resolved",
      (error: unknown) => String(error),
    );

    expect(result).toContain("protection capability could not be verified");
    expect(result).not.toContain("secret-bearer-token");
    expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    expect(localStorage.removeItem).not.toHaveBeenCalled();
  });

  it.each([
    { name: "absent", legacy: null },
    { name: "blank", legacy: "   " },
  ])("publishes native-only catalog operations for a $name renderer value", async ({ legacy }) => {
    const storage = new Map<string, string>();
    if (legacy !== null) storage.set("bibcode.connectionCatalog", legacy);
    const localStorage = {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
    };
    const compareAndSetConnectionCatalog = vi.fn(() => true);
    installTauriHarness({
      nativeConnectionCatalog: null,
      compareAndSetConnectionCatalog,
      compareConnectionCatalog: (expected) => expected === null,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);

    const bridge = await installBridge();

    await expect(bridge.getConnectionCatalog!()).resolves.toBeNull();
    await expect(bridge.compareConnectionCatalog!(null)).resolves.toBe(true);
    expect(compareAndSetConnectionCatalog).not.toHaveBeenCalled();
    expect(storage.has("bibcode.connectionCatalog")).toBe(false);
  });

  it("omits native catalog mutation methods when protected CAS is unavailable", async () => {
    installTauriHarness({ protectedConnectionCatalog: false });
    const bridge = await installBridge();

    expect(bridge.getConnectionCatalog).toBeDefined();
    expect(bridge.clearConnectionCatalog).toBeDefined();
    expect(bridge.setConnectionCatalog).toBeUndefined();
    expect(bridge.compareAndSetConnectionCatalog).toBeUndefined();
    expect(bridge.compareConnectionCatalog).toBeUndefined();
  });

  effectIt.effect.each([
    {
      name: "the metadata command rejects",
      options: {
        rejectMetadata: true,
        metadataError: new Error(
          "metadata failed with secret-bearer-token at /Users/alice/private/catalog.json",
        ),
      },
    },
    {
      name: "the host reports bridge version 1",
      options: { bridgeVersion: 1 },
    },
    {
      name: "the host reports bridge version 2 without compare-only support",
      options: { bridgeVersion: 2 },
    },
    {
      name: "the protected-catalog feature is missing",
      options: { omitProtectedConnectionCatalog: true },
    },
  ])("fails catalog mutation closed when $name", ({ options }) =>
    Effect.gen(function* () {
      const indexedDb = new IDBFactory();
      vi.stubGlobal("indexedDB", indexedDb);
      let nativeCatalog: string | null = SENSITIVE_CONNECTION_CATALOG;
      const browserStorage = new Map<string, string>([
        ["bibcode.connectionCatalog", SENSITIVE_CONNECTION_CATALOG],
      ]);
      const localStorage = {
        getItem: vi.fn((key: string) => browserStorage.get(key) ?? null),
        setItem: vi.fn((key: string, value: string) => browserStorage.set(key, value)),
        removeItem: vi.fn((key: string) => browserStorage.delete(key)),
      };
      const harness = installTauriHarness({
        ...options,
        nativeConnectionCatalog: () => nativeCatalog,
        compareAndSetConnectionCatalog: (expected, next) => {
          if (nativeCatalog !== expected) return false;
          nativeCatalog = next;
          return true;
        },
        clearConnectionCatalog: () => {
          nativeCatalog = null;
        },
      });
      Object.assign(window, { localStorage });
      vi.stubGlobal("localStorage", localStorage);
      yield* Effect.promise(() => installBridge());

      const result = yield* Effect.gen(function* () {
        const identityStore = yield* AcceptedStorageIdentityStore;
        yield* identityStore.accept({
          targetKey: "platform:primary",
          storageInstanceId: "must-not-be-persisted",
        });
      }).pipe(Effect.provide(connectionStorageLayer), Effect.result);

      expect
        .soft(yield* Effect.promise(() => readIndexedDbConnectionCatalog(indexedDb)))
        .toBeNull();
      expect.soft(nativeCatalog).toBe(SENSITIVE_CONNECTION_CATALOG);
      expect
        .soft(browserStorage.get("bibcode.connectionCatalog"))
        .toBe(SENSITIVE_CONNECTION_CATALOG);
      expect
        .soft(harness.invoke)
        .not.toHaveBeenCalledWith("desktop_bridge_clear_connection_catalog", undefined);
      expect(result._tag).toBe("Failure");
      if (result._tag === "Failure") {
        expect(result.failure).toBeInstanceOf(ConnectionTransientError);
        expect(result.failure.message).toContain("Could not migrate the local connection catalog");
        expect(result.failure.message).not.toContain("secret-bearer-token");
        expect(result.failure.message).not.toContain("secret-dpop-token");
        expect(result.failure.message).not.toContain("/Users/alice");
        expect(result.failure.message).not.toContain("private.example.test");
      }
    }),
  );

  it("normalizes structured unsupported errors returned by Tauri commands", async () => {
    installTauriHarness({ rejectSshProvisioning: true });
    const bridge = await installBridge();

    await expect(
      bridge.ensureSshEnvironment({
        alias: "host-2",
        hostname: "example.test",
        username: null,
        port: null,
      }),
    ).rejects.toMatchObject({
      name: "TauriDesktopCapabilityUnsupportedError",
      code: "tauri_capability_unsupported",
      method: "ensureSshEnvironment",
      capability: "sshProvisioning",
    });
  });

  it("routes context menus through the Tauri host command", async () => {
    const harness = installTauriHarness({ contextMenuResult: "open" });
    const bridge = await installBridge();
    const items = [{ id: "open", label: "Open" }] as const;

    await expect(bridge.showContextMenu(items, { x: 10, y: 20 })).resolves.toBe("open");

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_show_context_menu", {
      items,
      position: { x: 10, y: 20 },
    });
    expect(showContextMenuFallbackMock).not.toHaveBeenCalled();
  });

  it("uses the web context menu when the Windows native popup cannot report failure", async () => {
    const harness = installTauriHarness({ contextMenuResult: null });
    vi.stubGlobal("navigator", {
      userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
    });
    showContextMenuFallbackMock.mockResolvedValue("delete-worktree");
    const bridge = await installBridge();
    const items = [{ id: "delete-worktree", label: "Delete Worktree" }] as const;

    await expect(bridge.showContextMenu(items, { x: 30, y: 40 })).resolves.toBe("delete-worktree");

    expect(harness.invoke).not.toHaveBeenCalledWith(
      "desktop_bridge_show_context_menu",
      expect.anything(),
    );
    expect(showContextMenuFallbackMock).toHaveBeenCalledWith(items, { x: 30, y: 40 });
  });

  it("falls back to the web context menu when Tauri reports native context menus unsupported", async () => {
    const harness = installTauriHarness({ rejectContextMenu: unsupportedContextMenuError });
    showContextMenuFallbackMock.mockResolvedValue("rename");
    const bridge = await installBridge();
    const items = [{ id: "rename", label: "Rename" }] as const;

    await expect(bridge.showContextMenu(items, { x: 30, y: 40 })).resolves.toBe("rename");

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_show_context_menu", {
      items,
      position: { x: 30, y: 40 },
    });
    expect(showContextMenuFallbackMock).toHaveBeenCalledWith(items, { x: 30, y: 40 });
  });

  it("does not hide unexpected Tauri context menu errors behind the web fallback", async () => {
    const hostError = new Error("native menu crashed");
    installTauriHarness({ rejectContextMenu: hostError });
    const bridge = await installBridge();

    await expect(bridge.showContextMenu([{ id: "open", label: "Open" }])).rejects.toBe(hostError);
    expect(showContextMenuFallbackMock).not.toHaveBeenCalled();
  });

  it("returns disabled updater results through Tauri commands", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(bridge.checkForUpdate()).resolves.toEqual({
      checked: false,
      state: { status: "disabled" },
    });
    await expect(bridge.downloadUpdate()).resolves.toEqual({
      accepted: false,
      completed: false,
      state: { status: "disabled" },
    });
    await expect(bridge.installUpdate()).resolves.toEqual({
      accepted: false,
      completed: false,
      state: { status: "disabled" },
    });
    await expect(bridge.installUpdate({ excludedEnvironmentIds: ["wsl:Ubuntu"] })).resolves.toEqual(
      {
        accepted: false,
        completed: false,
        state: { status: "disabled" },
      },
    );
    await expect(bridge.installUpdate({ skipProtection: true })).resolves.toEqual({
      accepted: false,
      completed: false,
      state: { status: "disabled" },
    });

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_check_for_update", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_download_update", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_install_update", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_install_update", {
      input: { excludedEnvironmentIds: ["wsl:Ubuntu"] },
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_install_update", {
      input: { skipProtection: true },
    });
  });

  it("saves diagnostic archives through the Tauri host", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(
      bridge.saveDiagnosticLogs?.("diagnostics.zip", new Uint8Array([0x50, 0x4b])),
    ).resolves.toBe("C:\\Users\\test\\Downloads\\diagnostics.zip");
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_save_diagnostic_logs", {
      filename: "diagnostics.zip",
      bytes: [0x50, 0x4b],
    });
  });

  it("picks files to upload through the Tauri host", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(bridge.pickFiles?.({ title: "Select files to upload" })).resolves.toEqual([
      "C:\\workspace\\demo\\a.txt",
      "C:\\workspace\\demo\\b.txt",
    ]);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_pick_files", {
      options: { title: "Select files to upload" },
    });
  });

  it("downloads a transfer URL to a folder through the Tauri host", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(
      bridge.downloadToFolder?.({
        url: "https://127.0.0.1:3773/api/transfers/a.b",
        directory: "C:\\Users\\test\\Downloads",
        fileName: "src.zip",
      }),
    ).resolves.toBe("C:\\Users\\test\\Downloads\\src.zip");
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_download_to_folder", {
      url: "https://127.0.0.1:3773/api/transfers/a.b",
      directory: "C:\\Users\\test\\Downloads",
      fileName: "src.zip",
    });
  });

  it("uploads a file to a transfer URL through the Tauri host", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(
      bridge.uploadFile?.({
        url: "https://127.0.0.1:3773/api/transfers/a.b?name=src.zip&overwrite=0",
        path: "C:\\workspace\\demo\\src.zip",
      }),
    ).resolves.toEqual({ status: 201, body: "{}" });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_upload_file", {
      url: "https://127.0.0.1:3773/api/transfers/a.b?name=src.zip&overwrite=0",
      path: "C:\\workspace\\demo\\src.zip",
    });
  });

  it("routes privileged project-data recovery using identifiers rather than renderer paths", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    await expect(bridge.getProjectDataStatuses?.()).resolves.toBeNull();
    await expect(
      bridge.restoreProjectData?.("primary", "26b6ca53-27d3-401a-b51f-d7bdf534081f"),
    ).resolves.toBeNull();
    await expect(bridge.startEmptyProjectData?.("primary")).resolves.toBeNull();
    await expect(bridge.retryProjectData?.("primary")).resolves.toBeNull();
    await expect(bridge.openProjectDataPath?.("primary")).resolves.toBeNull();
    await expect(bridge.exportProjectDataDiagnostics?.("primary")).resolves.toBeNull();

    expect(harness.invoke).toHaveBeenCalledWith(
      "desktop_bridge_get_project_data_statuses",
      undefined,
    );
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_restore_project_data", {
      environmentId: "primary",
      backupId: "26b6ca53-27d3-401a-b51f-d7bdf534081f",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_start_empty_project_data", {
      environmentId: "primary",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_retry_project_data", {
      environmentId: "primary",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_open_project_data_path", {
      environmentId: "primary",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_export_project_data_diagnostics", {
      environmentId: "primary",
    });
  });

  it("restarts BiBCode through the guarded desktop command", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();

    expect(bridge.restartApp).toBeTypeOf("function");
    await bridge.restartApp!();
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_restart_app", undefined);
  });

  it("forwards project data status invalidations and disposes the native listener", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();
    const received: unknown[] = [];

    const dispose = bridge.onProjectDataStatusChanged?.((event) => received.push(event));
    expect(dispose).toEqual(expect.any(Function));
    await Promise.resolve();

    expect(harness.listen).toHaveBeenCalledWith(
      "desktop:project-data-status-changed",
      expect.any(Function),
    );
    harness.listeners.get("desktop:project-data-status-changed")?.({
      payload: { environmentId: "primary" },
    });
    expect(received).toEqual([{ environmentId: "primary" }]);

    dispose?.();
    await Promise.resolve();
    expect(harness.unlisteners.get("desktop:project-data-status-changed")).toHaveBeenCalledTimes(1);
  });

  it("routes the remaining desktop bridge capabilities through Tauri commands", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();
    const sshTarget = {
      alias: "host-1",
      hostname: "example.test",
      username: null,
      port: null,
    };

    await expect(bridge.getClientSettings()).resolves.toBeNull();
    await expect(bridge.setClientSettings({} as never)).resolves.toBeNull();
    await expect(bridge.discoverSshHosts()).resolves.toBeNull();
    await expect(bridge.disconnectSshEnvironment(sshTarget)).resolves.toBeNull();
    await expect(bridge.sshForward(sshTarget, 5173)).resolves.toBe(45123);
    await expect(bridge.sshForward(sshTarget, 41000, 5173)).resolves.toBe(45123);
    await expect(bridge.releaseSshForward(sshTarget, 5173)).resolves.toBeNull();
    await expect(bridge.resolveSshPasswordPrompt?.("request-1", "secret")).resolves.toBeNull();
    await expect(bridge.getServerExposureState()).resolves.toBeNull();
    await expect(bridge.applyServerExposure("network-accessible")).resolves.toBeNull();
    await expect(
      bridge.setTailscaleServeEnabled({ enabled: true, port: 8443 }),
    ).resolves.toBeNull();
    await expect(bridge.getAdvertisedEndpoints()).resolves.toBeNull();
    await expect(bridge.getWslState()).resolves.toBeNull();
    await expect(bridge.setWslBackendEnabled(true)).resolves.toBeNull();
    await expect(bridge.setWslDistro("Ubuntu")).resolves.toBeNull();
    await expect(bridge.setWslOnly(true)).resolves.toBeNull();
    await expect(bridge.pickFolder({ initialPath: "/workspace" })).resolves.toBeNull();
    await expect(bridge.confirm("Continue?")).resolves.toBeNull();
    await expect(bridge.setTheme("dark")).resolves.toBeNull();
    await expect(bridge.openExternal("https://example.test")).resolves.toBeNull();
    await expect(
      (
        bridge as DesktopBridge & {
          openInFileManager: (path: string, isDirectory: boolean) => Promise<void>;
        }
      ).openInFileManager("C:\\workspace\\demo\\src", true),
    ).resolves.toBeNull();
    await expect(bridge.getUpdateState()).resolves.toBeNull();

    const passwordRequests: unknown[] = [];
    const disposePasswordPrompt = bridge.onSshPasswordPrompt?.((request) =>
      passwordRequests.push(request),
    );
    await Promise.resolve();
    harness.listeners.get("desktop:ssh-password-prompt")?.({
      payload: { requestId: "request-1", prompt: "Password" },
    });
    expect(passwordRequests).toEqual([{ requestId: "request-1", prompt: "Password" }]);
    disposePasswordPrompt?.();

    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_get_client_settings", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_discover_ssh_hosts", undefined);
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_disconnect_ssh_environment", {
      target: sshTarget,
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_ssh_forward", {
      target: sshTarget,
      remotePort: 5173,
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_ssh_forward", {
      target: sshTarget,
      remotePort: 41000,
      preferredLocalPort: 5173,
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_release_ssh_forward", {
      target: sshTarget,
      remotePort: 5173,
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_apply_server_exposure", {
      desired: "network-accessible",
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_set_wsl_backend_enabled", {
      enabled: true,
    });
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_open_in_file_manager", {
      path: "C:\\workspace\\demo\\src",
      isDirectory: true,
    });
  });

  it("wraps Tauri event listeners and tears them down", async () => {
    const harness = installTauriHarness();
    const bridge = await installBridge();
    const menuActions: string[] = [];
    const updateStates: unknown[] = [];

    const disposeMenu = bridge.onMenuAction((action) => menuActions.push(action));
    const disposeUpdate = bridge.onUpdateState((state) => updateStates.push(state));
    await Promise.resolve();

    expect(harness.listen).toHaveBeenCalledWith("desktop:menu-action", expect.any(Function));
    expect(harness.listen).toHaveBeenCalledWith("desktop:update-state", expect.any(Function));

    harness.listeners.get("desktop:menu-action")?.({ payload: "open-settings" });
    harness.listeners.get("desktop:update-state")?.({
      payload: { status: "checking" },
    });

    expect(menuActions).toEqual(["open-settings"]);
    expect(updateStates).toEqual([{ status: "checking" }]);

    disposeMenu();
    disposeUpdate();
    await Promise.resolve();

    expect(harness.unlisteners.get("desktop:menu-action")).toHaveBeenCalledTimes(1);
    expect(harness.unlisteners.get("desktop:update-state")).toHaveBeenCalledTimes(1);
  });

  effectIt.effect(
    "keeps typed pairing requests authorized when another consumer refreshes the bridge after backend-ready",
    () =>
      Effect.gen(function* () {
        let bootstraps = [{ ...defaultLocalEnvironmentBootstrap, bootstrapToken: "bootstrap-1" }];
        const harness = installTauriHarness({ localEnvironmentBootstraps: () => bootstraps });
        Object.defineProperty(window, "location", {
          configurable: true,
          value: { origin: "tauri://localhost", href: "tauri://localhost/settings" },
        });
        let exchanges = 0;
        let currentBearer: string | null = null;
        // The native issuer keeps only its latest desktop bearer session. This inert
        // transport enforces that contract; the bridge, event and HTTP client are real.
        const fetchFixture = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
          const request = new Request(input, init);
          const pathname = new URL(request.url).pathname;
          if (pathname === "/oauth/token") {
            const form = new URLSearchParams(await request.text());
            expect(form.get("subject_token")).toBe(bootstraps[0]?.bootstrapToken);
            currentBearer = `issued-bearer-${++exchanges}`;
            return Response.json({ access_token: currentBearer });
          }
          if (request.headers.get("authorization") !== `Bearer ${currentBearer}`) {
            return Response.json(
              {
                _tag: "EnvironmentAuthInvalidError",
                code: "auth_invalid",
                reason: "invalid_credential",
                traceId: "superseded-session",
              },
              { status: 401 },
            );
          }
          if (pathname === "/api/auth/pairing-offer/cancel") {
            return Response.json({ cancelled: true });
          }
          if (pathname === "/api/auth/pairing-offer") {
            return Response.json({
              id: "offer-1",
              code: "pairing-offer",
              reach: "another-device",
              endpoint: defaultLocalEnvironmentBootstrap.httpBaseUrl,
              name: "Shared host",
              expiresAt: "2099-01-01T00:00:00Z",
            });
          }
          throw new Error("Unexpected inert auth request.");
        });
        vi.stubGlobal("fetch", fetchFixture);
        const bridge = yield* Effect.promise(installBridge);
        const { readDesktopPrimaryBearerToken } = yield* Effect.promise(
          () => import("./environments/primary/desktopAuth"),
        );
        const client = yield* Effect.promise(() => import("./environments/primary/httpClient"));
        const { makePrimaryEnvironmentHttpLayer } = yield* Effect.promise(
          () => import("./environments/primary/httpLayer"),
        );
        return yield* Effect.gen(function* () {
          const api = yield* client.PrimaryEnvironmentHttpClient;
          const offerInput = {
            name: "Shared host",
            endpoint: defaultLocalEnvironmentBootstrap.httpBaseUrl,
            reach: "another-device" as const,
          };
          const [first, directBefore] = yield* Effect.promise(() =>
            Promise.all([readDesktopPrimaryBearerToken(), bridge.getLocalEnvironmentBearerToken()]),
          );
          expect(directBefore).toBe(first);
          expect(exchanges).toBe(1);
          const before = yield* api.auth.pairingOffer({
            headers: { "idempotency-key": "before-ready" },
            payload: offerInput,
          });
          expect(before.id).toBe("offer-1");
          expect(
            yield* api.auth.cancelPairingOffer({
              headers: {},
              payload: { idempotencyKey: "before-ready" },
            }),
          ).toEqual({ cancelled: true });

          bootstraps = [{ ...defaultLocalEnvironmentBootstrap, bootstrapToken: "bootstrap-2" }];
          const backendReady = harness.listeners.get("desktop:backend-ready");
          expect(backendReady).toBeTypeOf("function");
          backendReady!({ payload: { reason: "restarted", bootstraps } });
          const refreshed = yield* Effect.promise(() => bridge.getLocalEnvironmentBearerToken());
          expect(refreshed).not.toBe(first);
          // The revoked-token control uses the same real typed response decoder.
          const stale = yield* makeEnvironmentHttpApiClient(
            defaultLocalEnvironmentBootstrap.httpBaseUrl,
          ).pipe(
            Effect.flatMap((raw) =>
              raw.auth.cancelPairingOffer({
                headers: { authorization: `Bearer ${first}` },
                payload: { idempotencyKey: "stale-control" },
              }),
            ),
            Effect.provide(remoteHttpClientLayer(fetchFixture)),
            Effect.result,
          );
          expect(stale).toMatchObject({
            _tag: "Failure",
            failure: { _tag: "EnvironmentAuthInvalidError", reason: "invalid_credential" },
          });

          const after = yield* api.auth.pairingOffer({
            headers: { "idempotency-key": "after-ready" },
            payload: offerInput,
          });
          expect(after.id).toBe("offer-1");
          expect(
            yield* api.auth.cancelPairingOffer({
              headers: {},
              payload: { idempotencyKey: "after-ready" },
            }),
          ).toEqual({ cancelled: true });
          const [primary, direct] = yield* Effect.promise(() =>
            Promise.all([readDesktopPrimaryBearerToken(), bridge.getLocalEnvironmentBearerToken()]),
          );
          expect(primary).toBe(refreshed);
          expect(direct).toBe(refreshed);
          expect(exchanges).toBe(2);
          expect(window.desktopBridge).toBe(bridge);
        }).pipe(
          Effect.provide(client.layer.pipe(Layer.provide(makePrimaryEnvironmentHttpLayer()))),
        );
      }),
  );

  it("retains the current bearer when an older exchange rejects after backend-ready", async () => {
    let bootstraps = [{ ...defaultLocalEnvironmentBootstrap, bootstrapToken: "first-bootstrap" }];
    const harness = installTauriHarness({ localEnvironmentBootstraps: () => bootstraps });
    let rejectFirst: ((error: Error) => void) | undefined;
    let exchanges = 0;
    vi.stubGlobal("fetch", async () => {
      exchanges++;
      if (exchanges === 1)
        return new Promise<Response>((_resolve, reject) => {
          rejectFirst = reject;
        });
      return Response.json({ access_token: `current-${exchanges}` });
    });
    const bridge = await installBridge();
    const first = bridge.getLocalEnvironmentBearerToken().catch((error: unknown) => error);
    await vi.waitFor(() => expect(rejectFirst).toBeTypeOf("function"));
    bootstraps = [{ ...defaultLocalEnvironmentBootstrap, bootstrapToken: "second-bootstrap" }];
    harness.listeners.get("desktop:backend-ready")!({
      payload: { reason: "restarted", bootstraps },
    });
    const replacement = await bridge.getLocalEnvironmentBearerToken();
    rejectFirst!(new Error("Old exchange rejected."));
    await first;
    const current = await bridge.getLocalEnvironmentBearerToken();
    expect(current).toBe(replacement);
    expect(exchanges).toBe(2);
  });

  it("shares a current exchange failure and coalesces its next retry across consumers", async () => {
    installTauriHarness();
    const failure = new Error("Current exchange failed.");
    let exchanges = 0;
    vi.stubGlobal("fetch", async () => {
      exchanges++;
      if (exchanges === 1) throw failure;
      return Response.json({ access_token: "recovered-bearer" });
    });
    const bridge = await installBridge();
    const { readDesktopPrimaryBearerToken } = await import("./environments/primary/desktopAuth");
    const rejected = await Promise.allSettled([
      readDesktopPrimaryBearerToken(),
      bridge.getLocalEnvironmentBearerToken(),
    ]);
    expect(rejected).toEqual([
      { status: "rejected", reason: failure },
      { status: "rejected", reason: failure },
    ]);
    expect(exchanges).toBe(1);
    await expect(
      Promise.all([readDesktopPrimaryBearerToken(), bridge.getLocalEnvironmentBearerToken()]),
    ).resolves.toEqual(["recovered-bearer", "recovered-bearer"]);
    expect(exchanges).toBe(2);
  });

  it("refreshes cached local bootstraps and bearer tokens from backend-ready events without reinstalling the bridge", async () => {
    const harness = installTauriHarness({
      localEnvironmentBootstraps: [
        {
          id: "primary",
          label: "Local",
          httpBaseUrl: "http://127.0.0.1:3773",
          wsBaseUrl: "ws://127.0.0.1:3773",
          bootstrapToken: "bootstrap-token-1",
        },
      ],
    });
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = new URLSearchParams(String(init?.body ?? ""));
      const subjectToken = body.get("subject_token");
      return {
        ok: true,
        json: async () => ({ access_token: `bearer-for-${subjectToken}` }),
      };
    });
    vi.stubGlobal("fetch", fetchMock);

    const bridge = await installBridge();
    await Promise.resolve();

    expect(harness.listen).toHaveBeenCalledWith("desktop:backend-ready", expect.any(Function));
    expect(await bridge.getLocalEnvironmentBearerToken()).toBe("bearer-for-bootstrap-token-1");
    expect(bridge.getLocalEnvironmentBootstraps()).toEqual([
      {
        id: "primary",
        label: "Local",
        httpBaseUrl: "http://127.0.0.1:3773",
        wsBaseUrl: "ws://127.0.0.1:3773",
        bootstrapToken: "bootstrap-token-1",
      },
    ]);

    const originalBridge = window.desktopBridge;
    harness.listeners.get("desktop:backend-ready")?.({
      payload: {
        reason: "restarted",
        bootstraps: [
          {
            id: "primary",
            label: "Local",
            httpBaseUrl: "http://127.0.0.1:4888",
            wsBaseUrl: "ws://127.0.0.1:4888",
            bootstrapToken: "bootstrap-token-2",
          },
        ],
      },
    });

    expect(window.desktopBridge).toBe(originalBridge);
    expect(bridge.getLocalEnvironmentBootstraps()).toEqual([
      {
        id: "primary",
        label: "Local",
        httpBaseUrl: "http://127.0.0.1:4888",
        wsBaseUrl: "ws://127.0.0.1:4888",
        bootstrapToken: "bootstrap-token-2",
      },
    ]);
    expect(await bridge.getLocalEnvironmentBearerToken()).toBe("bearer-for-bootstrap-token-2");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("uses browser fallbacks when optional native capabilities reject", async () => {
    const storage = new Map<string, string>([["bibcode.connectionCatalog", "browser-catalog"]]);
    const localStorage = {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
    };
    const harness = installTauriHarness({
      rejectFallbackCommands: true,
      protectedConnectionCatalog: false,
    });
    Object.assign(window, {
      confirm: vi.fn(() => true),
      open: vi.fn(),
      localStorage,
    });
    vi.stubGlobal("localStorage", localStorage);
    const bridge = await installBridge();

    await expect(bridge.getClientSettings()).resolves.toBeNull();
    await expect(bridge.setClientSettings(DEFAULT_CLIENT_SETTINGS)).resolves.toBeUndefined();
    await expect(bridge.getClientSettings()).resolves.toEqual(DEFAULT_CLIENT_SETTINGS);
    await expect(bridge.getConnectionCatalog!()).resolves.toBe("browser-catalog");
    expect(bridge.setConnectionCatalog).toBeUndefined();
    expect(bridge.compareAndSetConnectionCatalog).toBeUndefined();
    await expect(bridge.clearConnectionCatalog!()).resolves.toBeUndefined();
    await expect(bridge.discoverSshHosts()).resolves.toEqual([]);
    await expect(
      bridge.disconnectSshEnvironment({
        alias: "fallback",
        hostname: "fallback.test",
        username: null,
        port: null,
      }),
    ).resolves.toBeUndefined();
    await expect(bridge.getAdvertisedEndpoints()).resolves.toEqual([]);
    await expect(bridge.pickFolder()).resolves.toBeNull();
    await expect(bridge.confirm("Continue?")).resolves.toBe(true);
    await expect(bridge.setTheme("dark")).resolves.toBeUndefined();
    await expect(bridge.openExternal("https://example.test/path")).resolves.toBe(true);
    await expect(bridge.openExternal("file:///tmp/secret")).resolves.toBe(false);
    await expect(bridge.openExternal("not a URL")).resolves.toBe(false);

    await expect(bridge.getServerExposureState()).resolves.toMatchObject({ mode: "local-only" });
    await expect(bridge.applyServerExposure("network-accessible")).rejects.toThrow(
      "unsupported fallback command: desktop_bridge_apply_server_exposure",
    );
    await expect(
      bridge.setTailscaleServeEnabled({ enabled: true, port: 8443 }),
    ).resolves.toMatchObject({ tailscaleServeEnabled: false });
    await expect(bridge.getWslState()).resolves.toMatchObject({ enabled: false, distros: [] });
    // WSL mutations propagate failures instead of pretending a default state:
    // a swallowed set_wsl_only error could leave the firewall rule open while
    // the UI reports local-only.
    await expect(bridge.setWslBackendEnabled(true)).rejects.toThrow(
      "unsupported fallback command: desktop_bridge_set_wsl_backend_enabled",
    );
    await expect(bridge.setWslDistro("Ubuntu")).rejects.toThrow(
      "unsupported fallback command: desktop_bridge_set_wsl_distro",
    );
    await expect(bridge.setWslOnly(true)).rejects.toThrow(
      "unsupported fallback command: desktop_bridge_set_wsl_only",
    );
    await expect(bridge.getUpdateState()).resolves.toEqual({
      enabled: false,
      status: "disabled",
      currentVersion: import.meta.env.APP_VERSION || "0.0.0",
      hostArch: "other",
      appArch: "other",
      runningUnderArm64Translation: false,
      availableVersion: null,
      downloadedVersion: null,
      downloadPercent: null,
      checkedAt: null,
      message: null,
      errorContext: null,
      canRetry: false,
      requestedBy: null,
    });

    expect(window.open).toHaveBeenCalledWith(
      "https://example.test/path",
      "_blank",
      "noopener,noreferrer",
    );
    expect(localStorage.removeItem).toHaveBeenCalled();
    expect(harness.invoke).toHaveBeenCalledWith("desktop_bridge_get_update_state", undefined);
  });

  it("fails closed when browser catalog storage and event subscription are unavailable", async () => {
    const localStorage = {
      getItem: vi.fn(() => {
        throw new Error("storage blocked");
      }),
      setItem: vi.fn(() => {
        throw new Error("storage blocked");
      }),
      removeItem: vi.fn(() => {
        throw new Error("storage blocked");
      }),
    };
    installTauriHarness({
      rejectFallbackCommands: true,
      rejectListeners: true,
      protectedConnectionCatalog: false,
    });
    Object.assign(window, { localStorage });
    vi.stubGlobal("localStorage", localStorage);
    const bridge = await installBridge();

    await expect(bridge.getConnectionCatalog!()).resolves.toBeNull();
    expect(bridge.setConnectionCatalog).toBeUndefined();
    await expect(bridge.clearConnectionCatalog!()).resolves.toBeUndefined();
    const dispose = bridge.onMenuAction(() => {
      throw new Error("listener must stay inactive");
    });
    dispose();
    await Promise.resolve();

    expect(localStorage.getItem).toHaveBeenCalled();
    expect(localStorage.removeItem).toHaveBeenCalled();
  });

  it("retries bearer-token exchange after HTTP and payload failures", async () => {
    installTauriHarness();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({ ok: false, status: 503 })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ access_token: "" }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ access_token: "recovered" }) });
    vi.stubGlobal("fetch", fetchMock);
    const bridge = await installBridge();

    await expect(bridge.getLocalEnvironmentBearerToken()).rejects.toThrowError(/failed: 503/u);
    await expect(bridge.getLocalEnvironmentBearerToken()).rejects.toThrowError(
      /did not include a token/u,
    );
    await expect(bridge.getLocalEnvironmentBearerToken()).resolves.toBe("recovered");
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});
