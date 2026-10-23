import tailwindcss from "@tailwindcss/vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import { defineProject } from "vite-plus/test/config";
import "vite-plus/test/config";
import { defineConfig } from "vite-plus";
import pkg from "./package.json" with { type: "json" };

import { loadRepoEnv } from "../../scripts/lib/public-config";
import { themeBootstrapPlugins } from "../../scripts/lib/theme-bootstrap";

const repoEnv = loadRepoEnv();
Object.assign(process.env, repoEnv);

const port = Number(process.env.PORT ?? 5733);
const host = process.env.HOST?.trim() || "localhost";
const configuredWsUrl = process.env.VITE_WS_URL?.trim();
const configuredRelayUrl = repoEnv.VITE_BIBCODE_RELAY_URL?.trim() || "";
const configuredClerkPublishableKey = repoEnv.VITE_CLERK_PUBLISHABLE_KEY?.trim() || "";
const configuredClerkJwtTemplate = repoEnv.VITE_CLERK_JWT_TEMPLATE?.trim() || "";
const configuredHostedAppChannel = process.env.VITE_HOSTED_APP_CHANNEL?.trim() || "";
const configuredAppVersion = process.env.APP_VERSION?.trim() || pkg.version;
const configuredHostedAppUrl = (() => {
  const explicitHostedAppUrl = process.env.VITE_HOSTED_APP_URL?.trim();
  if (explicitHostedAppUrl) {
    return explicitHostedAppUrl;
  }
  if (process.env.VERCEL_ENV === "production" && process.env.VERCEL_PROJECT_PRODUCTION_URL) {
    return `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`;
  }
  if (process.env.VERCEL_URL) {
    return `https://${process.env.VERCEL_URL}`;
  }
  return undefined;
})();
const sourcemapEnv = process.env.BIBCODE_WEB_SOURCEMAP?.trim().toLowerCase();

const buildSourcemap =
  sourcemapEnv === "hidden" ? "hidden" : sourcemapEnv === "1" || sourcemapEnv === "true";

const unitTestProject = {
  extends: true,
  test: {
    name: "unit",
    include: ["src/**/*.test.{ts,tsx}"],
    // The web runtime suite exercises auth bootstrap, saved environments,
    // and websocket subscription lifecycles. Under the full monorepo test
    // run, those async tests can exceed Vitest's default 5s budget.
    hookTimeout: 15_000,
    testTimeout: 15_000,
  },
};

function resolveDevProxyTarget(wsUrl) {
  if (!wsUrl) {
    return undefined;
  }

  try {
    const url = new URL(wsUrl);
    if (url.protocol === "ws:") {
      url.protocol = "http:";
    } else if (url.protocol === "wss:") {
      url.protocol = "https:";
    }
    url.pathname = "";
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return undefined;
  }
}

const devProxyTarget = resolveDevProxyTarget(configuredWsUrl);

export default defineConfig(({ mode } = {}) => {
  return {
    plugins: [
      ...themeBootstrapPlugins(),
      tanstackRouter({
        // Unit tests inspect route component behavior directly. Keep the source
        // components available instead of replacing them with lazy wrappers.
        autoCodeSplitting: mode !== "test",
        routeFileIgnorePattern: "\\.test\\.(ts|tsx)$",
      }),
      react(),
      babel({
        // We need to be explicit about the parser options after moving to @vitejs/plugin-react v6.0.0
        // This is because the babel plugin only automatically parses typescript and jsx based on relative paths (e.g. "**/*.ts")
        // whereas the previous version of the plugin parsed all files with a .ts extension.
        // This is causing our packages/ directory to fail to parse, as they are not relative to the CWD.
        parserOpts: { plugins: ["typescript", "jsx"] },
        presets: [reactCompilerPreset()],
      }),
      tailwindcss(),
    ],
    optimizeDeps: {
      include: [
        "@clerk/react/internal",
        "@pierre/diffs",
        "@pierre/diffs/edit",
        "@pierre/diffs/react",
        "@pierre/diffs/worker/worker.js",
        "effect/Array",
        "effect/Order",
        "react-dom/client",
      ],
    },
    define: {
      // In dev mode, tell the web app where the WebSocket server lives
      "import.meta.env.VITE_WS_URL": JSON.stringify(configuredWsUrl ?? ""),
      "import.meta.env.VITE_BIBCODE_RELAY_URL": JSON.stringify(configuredRelayUrl),
      "import.meta.env.VITE_CLERK_PUBLISHABLE_KEY": JSON.stringify(configuredClerkPublishableKey),
      "import.meta.env.VITE_CLERK_JWT_TEMPLATE": JSON.stringify(configuredClerkJwtTemplate),
      "import.meta.env.VITE_HOSTED_APP_URL": JSON.stringify(configuredHostedAppUrl ?? ""),
      "import.meta.env.VITE_HOSTED_APP_CHANNEL": JSON.stringify(configuredHostedAppChannel),
      "import.meta.env.APP_VERSION": JSON.stringify(configuredAppVersion),
    },
    resolve: {
      tsconfigPaths: true,
      dedupe: ["react", "react-dom"],
    },
    server: {
      host,
      port,
      strictPort: true,
      ...(devProxyTarget
        ? {
            proxy: {
              "/.well-known": {
                target: devProxyTarget,
                changeOrigin: true,
              },
              "/api": {
                target: devProxyTarget,
                changeOrigin: true,
              },
              "/attachments": {
                target: devProxyTarget,
                changeOrigin: true,
              },
            },
          }
        : {}),
      hmr: {
        // Explicit config so Vite's HMR WebSocket connects reliably inside
        // the Tauri WebView. Vite 8 uses console.debug for
        // connection logs — enable "Verbose" in DevTools to see them.
        protocol: "ws",
        host,
        clientPort: port,
      },
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
      sourcemap: buildSourcemap,
      chunkSizeWarningLimit: 1536,
      // Keep production build logs actionable; Rolldown documents this as a
      // diagnostic timing warning rather than a correctness check.
      rolldownOptions: {
        checks: {
          pluginTimings: false,
        },
      },
    },
    test: {
      // Vitest v4 compatibility: preserve mock call history.
      // Remove after tests no longer rely on calls from setup or earlier tests.
      // https://viteplus.dev/guide/vitest-v5#remove-unneeded-compatibility-settings
      // https://vitest.dev/guide/migration/#clearmocks-is-enabled-by-default
      clearMocks: false,
      // Vitest v4 compatibility: keep separate Vite servers for inline projects.
      // Remove when plugins and config hooks can run once for shared projects.
      // https://viteplus.dev/guide/vitest-v5#remove-unneeded-compatibility-settings
      // https://vitest.dev/guide/migration/#inline-projects-share-the-vite-server-by-default
      sharedViteServer: false,
      projects: [defineProject(unitTestProject)],
    },
  };
});
