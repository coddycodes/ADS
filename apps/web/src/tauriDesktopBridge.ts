import type {
  AuthAccessTokenResult,
  ClientSettings,
  ContextMenuEntry,
  DesktopAppBranding,
  DesktopBridge,
  DesktopBridgeHostMetadata,
  DesktopEnvironmentBootstrap,
  DesktopProjectDataStatusChangedEvent,
  DesktopServerExposureState,
  DesktopSshPasswordPromptRequest,
  DesktopUpdateActionResult,
  DesktopUpdateCheckResult,
  DesktopUpdateInstallInput,
  DesktopUpdateState,
  DesktopWslState,
} from "@bibcode/contracts";
import {
  AuthAccessTokenType,
  AuthEnvironmentBootstrapTokenType,
  AuthTokenExchangeGrantType,
  DESKTOP_DEEP_LINK_EVENT,
  PRIMARY_LOCAL_ENVIRONMENT_ID,
} from "@bibcode/contracts";
import { invoke as importedTauriInvoke, isTauri as isImportedTauri } from "@tauri-apps/api/core";
import { listen as importedTauriListen } from "@tauri-apps/api/event";

import { startBrowserSurfaceSync } from "./browser/browserSurfaceSync";
import { formatAppDisplayName } from "./branding.logic";
import { readBrowserClientSettings, writeBrowserClientSettings } from "./clientPersistenceStorage";
import { showContextMenuFallback } from "./contextMenuFallback";
import { invokeTauriCommand, type TauriCommandMock } from "./tauriInvokeRouting";
import { createTauriPreviewBridge } from "./tauriPreviewBridge";

const CONNECTION_CATALOG_STORAGE_KEY = "bibcode.connectionCatalog";
const BACKEND_READY_EVENT = "desktop:backend-ready";
const PROJECT_DATA_STATUS_CHANGED_EVENT = "desktop:project-data-status-changed";
const MENU_ACTION_EVENT = "desktop:menu-action";
const NIGHTLY_VERSION_PATTERN = /-nightly\.\d{8}\.\d+$/;
const SSH_PASSWORD_PROMPT_EVENT = "desktop:ssh-password-prompt";
const UPDATE_STATE_EVENT = "desktop:update-state";
const LOCAL_ENVIRONMENT_BOOTSTRAP_TIMEOUT_MS = 15_000;
const LOCAL_ENVIRONMENT_BOOTSTRAP_RETRY_MS = 50;
const PROTECTED_CONNECTION_CATALOG_BRIDGE_VERSION = 3;

type ConnectionCatalogProtectionCapability = "protected" | "unprotected" | "unknown";

let cachedLocalEnvironmentBootstraps: readonly DesktopEnvironmentBootstrap[] = [];
let localEnvironmentBootstrapsRefresh: Promise<readonly DesktopEnvironmentBootstrap[]> | null =
  null;
let localEnvironmentBearerToken: Promise<string> | null = null;

interface TauriDesktopBackendReadyPayload {
  readonly reason: "started" | "restarted";
  readonly bootstraps: readonly DesktopEnvironmentBootstrap[];
}

interface TauriDesktopCapabilityUnsupportedPayload {
  readonly code: "tauri_capability_unsupported";
  readonly method: string;
  readonly capability: string;
  readonly message?: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isTauriDesktopCapabilityUnsupportedPayload(
  value: unknown,
): value is TauriDesktopCapabilityUnsupportedPayload {
  return (
    isRecord(value) &&
    value.code === "tauri_capability_unsupported" &&
    typeof value.method === "string" &&
    typeof value.capability === "string" &&
    (value.message === undefined || typeof value.message === "string")
  );
}

function tauriInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const invoke = window.__TAURI__?.core?.invoke;
  const registeredMock =
    import.meta.env.VITE_BIBCODE_DESKTOP_E2E === "1" ? window.__wdio_mocks__?.[command] : undefined;
  return invokeTauriCommand<T>({
    command,
    args,
    ...(typeof registeredMock === "function"
      ? { e2eMock: registeredMock as TauriCommandMock }
      : {}),
    ...(invoke
      ? {
          globalInvoke: (invokeCommand, invokeArgs) => invoke<unknown>(invokeCommand, invokeArgs),
        }
      : {}),
    importedInvoke: (invokeCommand, invokeArgs) =>
      importedTauriInvoke<unknown>(invokeCommand, invokeArgs),
  });
}

function tauriListen<T>(event: string, listener: (payload: T) => void): () => void {
  const listen = window.__TAURI__?.event?.listen;
  const subscribe = listen ?? importedTauriListen;

  let active = true;
  const unlisten = subscribe<T>(event, ({ payload }) => {
    if (active) listener(payload);
  }).catch(() => undefined);

  return () => {
    active = false;
    void unlisten.then((dispose) => dispose?.());
  };
}

async function tauriInvokeOr<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  fallback: () => T | Promise<T>,
): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args);
  } catch {
    return fallback();
  }
}

export class TauriDesktopCapabilityUnsupportedError extends Error {
  readonly code = "tauri_capability_unsupported";

  constructor(
    readonly method: string,
    readonly capability: string,
    message = `${method} requires ${capability}, which is not implemented by the Tauri desktop host yet.`,
  ) {
    super(message);
    this.name = "TauriDesktopCapabilityUnsupportedError";
  }
}

function normalizeTauriDesktopError(error: unknown): unknown {
  if (isTauriDesktopCapabilityUnsupportedPayload(error)) {
    return new TauriDesktopCapabilityUnsupportedError(
      error.method,
      error.capability,
      error.message,
    );
  }
  return error;
}

async function tauriInvokeDesktop<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args);
  } catch (error) {
    throw normalizeTauriDesktopError(error);
  }
}

function refreshLocalEnvironmentBootstraps(): Promise<readonly DesktopEnvironmentBootstrap[]> {
  localEnvironmentBootstrapsRefresh ??= tauriInvoke<DesktopEnvironmentBootstrap[]>(
    "desktop_bridge_get_local_environment_bootstraps",
  )
    .then((bootstraps) => {
      cachedLocalEnvironmentBootstraps = bootstraps;
      return bootstraps;
    })
    .finally(() => {
      localEnvironmentBootstrapsRefresh = null;
    });
  return localEnvironmentBootstrapsRefresh;
}

function getCachedLocalEnvironmentBootstraps(): readonly DesktopEnvironmentBootstrap[] {
  void refreshLocalEnvironmentBootstraps().catch(() => undefined);
  return cachedLocalEnvironmentBootstraps;
}

function applyBackendReady(payload: TauriDesktopBackendReadyPayload): void {
  cachedLocalEnvironmentBootstraps = payload.bootstraps;
  localEnvironmentBootstrapsRefresh = null;
  localEnvironmentBearerToken = null;
}

function primaryBootstrapFrom(
  bootstraps: readonly DesktopEnvironmentBootstrap[],
): DesktopEnvironmentBootstrap | null {
  return (
    bootstraps.find(
      (bootstrap) =>
        bootstrap.id === PRIMARY_LOCAL_ENVIRONMENT_ID &&
        typeof bootstrap.httpBaseUrl === "string" &&
        typeof bootstrap.bootstrapToken === "string",
    ) ?? null
  );
}

async function getPrimaryLocalEnvironmentBootstrap(): Promise<DesktopEnvironmentBootstrap> {
  const deadline = Date.now() + LOCAL_ENVIRONMENT_BOOTSTRAP_TIMEOUT_MS;

  while (true) {
    const cached = primaryBootstrapFrom(cachedLocalEnvironmentBootstraps);
    if (cached) {
      return cached;
    }

    const refreshed = primaryBootstrapFrom(await refreshLocalEnvironmentBootstraps());
    if (refreshed) {
      return refreshed;
    }

    if (Date.now() >= deadline) {
      throw new Error("Tauri local environment bootstrap is not available.");
    }

    await new Promise<void>((resolve) => {
      globalThis.setTimeout(resolve, LOCAL_ENVIRONMENT_BOOTSTRAP_RETRY_MS);
    });
  }
}

async function exchangeLocalEnvironmentBearerToken(): Promise<string> {
  const bootstrap = await getPrimaryLocalEnvironmentBootstrap();
  const httpBaseUrl = bootstrap.httpBaseUrl;
  const credential = bootstrap.bootstrapToken;
  if (typeof httpBaseUrl !== "string" || typeof credential !== "string") {
    throw new Error("Tauri local environment bootstrap is incomplete.");
  }

  const body = new URLSearchParams({
    grant_type: AuthTokenExchangeGrantType,
    subject_token: credential,
    subject_token_type: AuthEnvironmentBootstrapTokenType,
    requested_token_type: AuthAccessTokenType,
    client_label: "BiBCode Tauri Desktop",
    client_device_type: "desktop",
  });
  const response = await fetch(new URL("/oauth/token", httpBaseUrl), {
    method: "POST",
    headers: {
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
  if (!response.ok) {
    throw new Error(`Tauri local environment bearer token exchange failed: ${response.status}`);
  }

  const result = (await response.json()) as Partial<AuthAccessTokenResult>;
  if (typeof result.access_token !== "string" || result.access_token.length === 0) {
    throw new Error("Tauri local environment bearer token response did not include a token.");
  }
  return result.access_token;
}

function getLocalEnvironmentBearerToken(): Promise<string> {
  if (localEnvironmentBearerToken === null) {
    const exchange: Promise<string> = exchangeLocalEnvironmentBearerToken().catch((error) => {
      if (localEnvironmentBearerToken === exchange) {
        localEnvironmentBearerToken = null;
      }
      throw error;
    });
    localEnvironmentBearerToken = exchange;
  }
  return localEnvironmentBearerToken;
}

function readLocalStorageConnectionCatalog(): string | null {
  try {
    return localStorage.getItem(CONNECTION_CATALOG_STORAGE_KEY);
  } catch {
    return null;
  }
}

function readProtectedLegacyConnectionCatalog(): string | null {
  const storage = (window as Window & { readonly localStorage?: Storage }).localStorage;
  return storage?.getItem(CONNECTION_CATALOG_STORAGE_KEY) ?? null;
}

function clearLocalStorageConnectionCatalog(): void {
  try {
    localStorage.removeItem(CONNECTION_CATALOG_STORAGE_KEY);
  } catch {}
}

async function getConnectionCatalog(): Promise<string | null> {
  const catalog = await tauriInvokeOr<string | null>(
    "desktop_bridge_get_connection_catalog",
    undefined,
    readLocalStorageConnectionCatalog,
  );
  return catalog ?? readLocalStorageConnectionCatalog();
}

async function getNativeConnectionCatalog(): Promise<string | null> {
  return tauriInvoke<string | null>("desktop_bridge_get_connection_catalog", undefined);
}

async function setNativeConnectionCatalog(catalog: string): Promise<boolean> {
  return tauriInvoke<boolean>("desktop_bridge_set_connection_catalog", { catalog });
}

async function compareAndSetNativeConnectionCatalog(
  expectedCatalog: string | null,
  nextCatalog: string,
): Promise<boolean> {
  return tauriInvoke<boolean>("desktop_bridge_compare_and_set_connection_catalog", {
    expectedCatalog,
    nextCatalog,
  });
}

async function compareNativeConnectionCatalog(expectedCatalog: string | null): Promise<boolean> {
  return tauriInvoke<boolean>("desktop_bridge_compare_connection_catalog", {
    expectedCatalog,
  });
}

async function clearNativeConnectionCatalog(): Promise<void> {
  await tauriInvoke("desktop_bridge_clear_connection_catalog", undefined);
}

async function establishNativeConnectionCatalogSource(): Promise<void> {
  const legacyCatalog = readProtectedLegacyConnectionCatalog();
  let nativeCatalog = await getNativeConnectionCatalog();

  if (nativeCatalog === null && legacyCatalog !== null && legacyCatalog.trim() !== "") {
    await compareAndSetNativeConnectionCatalog(null, legacyCatalog);
    nativeCatalog = await getNativeConnectionCatalog();
    if (nativeCatalog === null) {
      throw new Error("Protected connection catalog migration could not be confirmed.");
    }
  }

  if (legacyCatalog !== null) {
    clearLocalStorageConnectionCatalog();
  }
}

async function clearConnectionCatalog(): Promise<void> {
  await tauriInvokeOr(
    "desktop_bridge_clear_connection_catalog",
    undefined,
    clearLocalStorageConnectionCatalog,
  );
  clearLocalStorageConnectionCatalog();
}

function unavailableConnectionCatalogOperation<T>(): Promise<T> {
  return Promise.reject(
    new Error("Desktop connection catalog protection capability could not be verified."),
  );
}

function connectionCatalogProtectionCapability(
  metadata: DesktopBridgeHostMetadata | null,
): ConnectionCatalogProtectionCapability {
  if (metadata?.bridgeVersion !== PROTECTED_CONNECTION_CATALOG_BRIDGE_VERSION) {
    return "unknown";
  }
  const protectedConnectionCatalog = metadata.features?.protectedConnectionCatalog;
  return protectedConnectionCatalog === true
    ? "protected"
    : protectedConnectionCatalog === false
      ? "unprotected"
      : "unknown";
}

function defaultServerExposureState(): DesktopServerExposureState {
  return {
    mode: "local-only",
    configuredMode: "local-only",
    management: "native",
    endpointUrl: null,
    advertisedHost: null,
    tailscaleServeEnabled: false,
    tailscaleServePort: 443,
  };
}

function defaultWslState(): DesktopWslState {
  return {
    enabled: false,
    distro: null,
    available: false,
    wslOnly: false,
    distros: [],
    preflightError: null,
  };
}

function defaultUpdateState(): DesktopUpdateState {
  return {
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
  };
}

function resolveTauriAppBranding(): DesktopAppBranding {
  const currentVersion = import.meta.env.APP_VERSION || "0.0.0";
  const stageLabel = import.meta.env.DEV
    ? "Dev"
    : NIGHTLY_VERSION_PATTERN.test(currentVersion)
      ? "Nightly"
      : "Latest";
  return {
    baseName: "BiBCode",
    stageLabel,
    displayName: formatAppDisplayName({ baseName: "BiBCode", stageLabel }),
  };
}

function openBrowserFallback(url: string): boolean {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return false;
    }
    window.open(parsed.href, "_blank", "noopener,noreferrer");
    return true;
  } catch {
    return false;
  }
}

function isWindowsWebViewRuntime(): boolean {
  return typeof navigator !== "undefined" && /Windows/iu.test(navigator.userAgent);
}

async function showTauriContextMenu<T extends string>(
  items: readonly ContextMenuEntry<T>[],
  position?: { x: number; y: number },
): Promise<T | null> {
  if (isWindowsWebViewRuntime()) {
    return showContextMenuFallback(items, position);
  }

  try {
    return await tauriInvokeDesktop<T | null>("desktop_bridge_show_context_menu", {
      items,
      position,
    });
  } catch (error) {
    if (
      error instanceof TauriDesktopCapabilityUnsupportedError &&
      error.method === "showContextMenu" &&
      error.capability === "nativeContextMenu"
    ) {
      return showContextMenuFallback(items, position);
    }
    throw error;
  }
}

function createTauriDesktopBridge(
  previewSupported: boolean,
  connectionCatalogProtection: ConnectionCatalogProtectionCapability,
): DesktopBridge {
  const preview = previewSupported
    ? createTauriPreviewBridge({
        invoke: tauriInvoke,
        listen: tauriListen,
      })
    : undefined;
  const connectionCatalog =
    connectionCatalogProtection === "protected"
      ? {
          getConnectionCatalog: getNativeConnectionCatalog,
          setConnectionCatalog: setNativeConnectionCatalog,
          compareConnectionCatalog: compareNativeConnectionCatalog,
          compareAndSetConnectionCatalog: compareAndSetNativeConnectionCatalog,
          clearConnectionCatalog: clearNativeConnectionCatalog,
        }
      : connectionCatalogProtection === "unprotected"
        ? { getConnectionCatalog, clearConnectionCatalog }
        : {
            getConnectionCatalog: () => unavailableConnectionCatalogOperation<string | null>(),
            clearConnectionCatalog: () => unavailableConnectionCatalogOperation<void>(),
          };

  return {
    getHostMetadata: () =>
      tauriInvoke<DesktopBridgeHostMetadata>("desktop_bridge_get_bridge_metadata", undefined),
    getAppBranding: resolveTauriAppBranding,
    getLocalEnvironmentBootstraps: getCachedLocalEnvironmentBootstraps,
    getLocalEnvironmentBearerToken,
    getClientSettings: () =>
      tauriInvokeOr<ClientSettings | null>("desktop_bridge_get_client_settings", undefined, () =>
        readBrowserClientSettings(),
      ),
    setClientSettings: (settings: ClientSettings) =>
      tauriInvokeOr("desktop_bridge_set_client_settings", { settings }, () =>
        writeBrowserClientSettings(settings),
      ),
    ...connectionCatalog,
    getProjectDataStatuses: () =>
      tauriInvokeDesktop("desktop_bridge_get_project_data_statuses", undefined),
    onProjectDataStatusChanged: (listener: (event: DesktopProjectDataStatusChangedEvent) => void) =>
      tauriListen(PROJECT_DATA_STATUS_CHANGED_EVENT, listener),
    getPendingDeepLinks: async () =>
      (await tauriInvoke<ReadonlyArray<string> | null>("plugin:deep-link|get_current")) ?? [],
    onDeepLink: (listener: (urls: ReadonlyArray<string>) => void) =>
      tauriListen(DESKTOP_DEEP_LINK_EVENT, listener),
    restoreProjectData: (environmentId, backupId) =>
      tauriInvokeDesktop("desktop_bridge_restore_project_data", { environmentId, backupId }),
    startEmptyProjectData: (environmentId) =>
      tauriInvokeDesktop("desktop_bridge_start_empty_project_data", { environmentId }),
    retryProjectData: (environmentId) =>
      tauriInvokeDesktop("desktop_bridge_retry_project_data", { environmentId }),
    restartApp: () => tauriInvokeDesktop("desktop_bridge_restart_app", undefined),
    openProjectDataPath: (environmentId) =>
      tauriInvokeDesktop("desktop_bridge_open_project_data_path", { environmentId }),
    exportProjectDataDiagnostics: (environmentId) =>
      tauriInvokeDesktop("desktop_bridge_export_project_data_diagnostics", { environmentId }),
    discoverSshHosts: () => tauriInvokeOr("desktop_bridge_discover_ssh_hosts", undefined, () => []),
    ensureSshEnvironment: (target, options) =>
      tauriInvokeDesktop("desktop_bridge_ensure_ssh_environment", { target, options }),
    disconnectSshEnvironment: (target) =>
      tauriInvokeOr("desktop_bridge_disconnect_ssh_environment", { target }, () => undefined),
    sshForward: (target, remotePort, preferredLocalPort) =>
      tauriInvokeDesktop("desktop_bridge_ssh_forward", { target, remotePort, preferredLocalPort }),
    releaseSshForward: (target, remotePort) =>
      tauriInvokeOr("desktop_bridge_release_ssh_forward", { target, remotePort }, () => undefined),
    fetchSshEnvironmentDescriptor: (httpBaseUrl: string) =>
      tauriInvoke("desktop_bridge_fetch_environment_descriptor", { httpBaseUrl }),
    bootstrapSshBearerSession: (httpBaseUrl, credential, scopes) =>
      tauriInvoke("desktop_bridge_bootstrap_ssh_bearer_session", {
        httpBaseUrl,
        credential,
        ...(scopes === undefined ? {} : { scopes }),
      }),
    fetchSshSessionState: (httpBaseUrl: string, bearerToken: string) =>
      tauriInvoke("desktop_bridge_fetch_ssh_session_state", { httpBaseUrl, bearerToken }),
    issueSshWebSocketTicket: (httpBaseUrl: string, bearerToken: string) =>
      tauriInvoke("desktop_bridge_issue_ssh_web_socket_ticket", { httpBaseUrl, bearerToken }),
    onSshPasswordPrompt: (listener: (request: DesktopSshPasswordPromptRequest) => void) =>
      tauriListen(SSH_PASSWORD_PROMPT_EVENT, listener),
    resolveSshPasswordPrompt: (requestId, password) =>
      tauriInvokeDesktop("desktop_bridge_resolve_ssh_password_prompt", { requestId, password }),
    getServerExposureState: () =>
      tauriInvokeOr<DesktopServerExposureState>(
        "desktop_bridge_get_server_exposure_state",
        undefined,
        defaultServerExposureState,
      ),
    applyServerExposure: (desired) =>
      tauriInvokeDesktop<DesktopServerExposureState>("desktop_bridge_apply_server_exposure", {
        desired,
      }),
    setTailscaleServeEnabled: (input) =>
      tauriInvokeOr<DesktopServerExposureState>(
        "desktop_bridge_set_tailscale_serve_enabled",
        { input },
        defaultServerExposureState,
      ),
    getAdvertisedEndpoints: () =>
      tauriInvokeOr("desktop_bridge_get_advertised_endpoints", undefined, () => []),
    getWslState: () =>
      tauriInvokeOr<DesktopWslState>("desktop_bridge_get_wsl_state", undefined, defaultWslState),
    // WSL mutations must propagate failures: a swallowed set_wsl_only error
    // can leave the Windows firewall rule open while the UI reports
    // local-only. The settings surfaces surface the rejection to the user.
    setWslBackendEnabled: (enabled) =>
      tauriInvoke<DesktopWslState>("desktop_bridge_set_wsl_backend_enabled", { enabled }),
    setWslDistro: (distro) =>
      tauriInvoke<DesktopWslState>("desktop_bridge_set_wsl_distro", { distro }),
    setWslOnly: (enabled) =>
      tauriInvoke<DesktopWslState>("desktop_bridge_set_wsl_only", { enabled }),
    pickFolder: (options) => tauriInvokeOr("desktop_bridge_pick_folder", { options }, () => null),
    saveDiagnosticLogs: (filename, bytes) =>
      tauriInvokeDesktop<string | null>("desktop_bridge_save_diagnostic_logs", {
        filename,
        bytes: Array.from(bytes),
      }),
    pickFiles: (options) =>
      tauriInvokeOr<readonly string[]>("desktop_bridge_pick_files", { options }, () => []),
    downloadToFolder: (input) =>
      tauriInvokeDesktop<string>("desktop_bridge_download_to_folder", {
        url: input.url,
        directory: input.directory,
        fileName: input.fileName,
      }),
    uploadFile: (input) =>
      tauriInvokeDesktop<{ status: number; body: string }>("desktop_bridge_upload_file", {
        url: input.url,
        path: input.path,
      }),
    confirm: (message) =>
      tauriInvokeOr("desktop_bridge_confirm", { message }, () => window.confirm(message)),
    setTheme: (theme) => tauriInvokeOr("desktop_bridge_set_theme", { theme }, () => undefined),
    showContextMenu: <T extends string>(
      items: readonly ContextMenuEntry<T>[],
      position?: { x: number; y: number },
    ) => showTauriContextMenu(items, position),
    openExternal: (url: string) =>
      tauriInvokeOr("desktop_bridge_open_external", { url }, () => openBrowserFallback(url)),
    openInFileManager: (path: string, isDirectory: boolean) =>
      tauriInvokeDesktop("desktop_bridge_open_in_file_manager", { path, isDirectory }),
    onMenuAction: (listener: (action: string) => void) => tauriListen(MENU_ACTION_EVENT, listener),
    getUpdateState: () =>
      tauriInvokeOr<DesktopUpdateState>(
        "desktop_bridge_get_update_state",
        undefined,
        defaultUpdateState,
      ),
    checkForUpdate: (): Promise<DesktopUpdateCheckResult> =>
      tauriInvokeDesktop("desktop_bridge_check_for_update", undefined),
    downloadUpdate: (): Promise<DesktopUpdateActionResult> =>
      tauriInvokeDesktop("desktop_bridge_download_update", undefined),
    installUpdate: (input?: DesktopUpdateInstallInput): Promise<DesktopUpdateActionResult> =>
      tauriInvokeDesktop(
        "desktop_bridge_install_update",
        input === undefined ? undefined : { input },
      ),
    onUpdateState: (listener: (state: DesktopUpdateState) => void) =>
      tauriListen(UPDATE_STATE_EVENT, listener),
    ...(preview ? { preview } : {}),
  };
}

const isTauriDesktopRuntime =
  typeof window !== "undefined" && (window.__TAURI__ !== undefined || isImportedTauri());

async function installTauriDesktopBridge(): Promise<void> {
  if (window.desktopBridge !== undefined) {
    return;
  }

  const metadata = await tauriInvoke<DesktopBridgeHostMetadata>(
    "desktop_bridge_get_bridge_metadata",
    undefined,
  ).catch(() => null);

  if (window.desktopBridge !== undefined) {
    return;
  }

  const declaredConnectionCatalogProtection = connectionCatalogProtectionCapability(metadata);
  const connectionCatalogProtection =
    declaredConnectionCatalogProtection === "protected"
      ? await establishNativeConnectionCatalogSource().then(
          () => declaredConnectionCatalogProtection,
          () => "unknown" as const,
        )
      : declaredConnectionCatalogProtection;

  if (window.desktopBridge !== undefined) {
    return;
  }

  const bridge = createTauriDesktopBridge(
    metadata?.features?.preview === true,
    connectionCatalogProtection,
  );
  window.desktopBridge = bridge;
  const preview = bridge.preview;
  if (preview) startBrowserSurfaceSync(preview);
  tauriListen<TauriDesktopBackendReadyPayload>(BACKEND_READY_EVENT, applyBackendReady);
}

export const tauriDesktopBridgeReady: Promise<void> = isTauriDesktopRuntime
  ? installTauriDesktopBridge()
      .then(() => getPrimaryLocalEnvironmentBootstrap())
      .then(() => undefined)
  : Promise.resolve();
