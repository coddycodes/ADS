/// <reference types="vite-plus/client" />

import type { DesktopBridge, LocalApi } from "@coddy/contracts";

interface ImportMetaEnv {
  readonly VITE_HTTP_URL: string;
  readonly VITE_WS_URL: string;
  readonly VITE_HOSTED_APP_URL: string;
  readonly VITE_HOSTED_APP_CHANNEL: string;
  readonly VITE_CLERK_PUBLISHABLE_KEY: string;
  readonly VITE_CLERK_JWT_TEMPLATE: string;
  readonly APP_VERSION: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare global {
  interface TauriCoreApi {
    invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  }

  interface TauriEventApi {
    listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void>;
  }

  interface TauriGlobalApi {
    core?: TauriCoreApi;
    event?: TauriEventApi;
  }

  interface Window {
    __TAURI__?: TauriGlobalApi;
    nativeApi?: LocalApi;
    desktopBridge?: DesktopBridge;
  }
}
