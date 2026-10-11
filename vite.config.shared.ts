import "vite-plus/test/config";
import { defineConfig } from "vite-plus";
import * as NodeURL from "node:url";

export const coverageInclude = [
  "vite.config.shared.ts",
  "apps/marketing/astro.config.mjs",
  "apps/marketing/src/**/*.ts",
  "apps/marketing/vercel.ts",
  "apps/web/src/**/*.{ts,tsx}",
  "apps/web/vercel.ts",
  "apps/web/vite.config.app.mjs",
  "infra/relay/alchemy.run.ts",
  "infra/relay/scripts/**/*.ts",
  "infra/relay/src/**/*.ts",
  "oxlint-plugin-bibcode/**/*.ts",
  "packages/client-runtime/scripts/**/*.ts",
  "packages/client-runtime/src/**/*.ts",
  "packages/client-runtime/vite.config.runtime.ts",
  "packages/contracts/scripts/**/*.ts",
  "packages/contracts/src/**/*.ts",
  "packages/shared/src/**/*.ts",
  "scripts/**/*.mjs",
  "scripts/**/*.ts",
] as const;

export const coverageExclude = [
  "**/*.d.ts",
  "**/*.spec.{ts,tsx,js,mjs}",
  "**/*.test.{ts,tsx,js,mjs}",
  "**/test/**",
  "**/__tests__/**",
  "**/vite.config.ts",
  "**/.repos/**",
  "**/.vite-plus/**",
  "**/coverage/**",
  "**/.vitest-coverage*/**",
  "**/dist/**",
  "**/node_modules/**",
  "**/target/**",
  "**/.{idea,git,cache,output,temp}/**",
  "apps/web/public/mockServiceWorker.js",
  // Classic browser script evaluated in node:vm by scripts/lib/theme-bootstrap.test.ts;
  // V8 coverage does not attribute those executions to this static asset's file path.
  "apps/web/public/theme-bootstrap.js",
  "apps/web/src/lib/vendor/qrcodegen.ts",
  "apps/web/src/routeTree.gen.ts",
] as const;

export const coverageProbeEntrypoints = [
  "vite.config.shared.ts",
  "apps/web/vite.config.app.mjs",
  "packages/client-runtime/vite.config.runtime.ts",
] as const;

const testExclude = [
  "**/.repos/**",
  "**/.worktrees/**",
  "**/node_modules/**",
  "**/dist/**",
  "**/.{idea,git,cache,output,temp}/**",
  // The root config runs no React plugins, so this React Compiler guard only passes in
  // the web package's lane (`vp run test`). See "Web unit tests and the React Compiler"
  // in docs/testing/README.md.
  "apps/web/src/reactCompiler.test.tsx",
  // These real-component fixtures use their own compiler and owned service ports.
  // Typography CI runs their dedicated config and complete caller checks.
  "apps/web/visual/typography/**",
] as const;

export default defineConfig({
  resolve: {
    alias: {
      "~": NodeURL.fileURLToPath(new URL("./apps/web/src", import.meta.url)),
    },
  },
  test: {
    environment: "node",
    exclude: testExclude,
    coverage: {
      provider: "v8",
      include: coverageInclude,
      exclude: coverageExclude,
      thresholds: {
        lines: 90,
        statements: 90,
        functions: 90,
        branches: 90,
      },
    },
    hookTimeout: 60_000,
    testTimeout: 60_000,
  },
  fmt: {
    ignorePatterns: [
      ".reference",
      ".repos/**",
      ".plans",
      "docs/superpowers/**",
      ".alchemy",
      "dist",
      "node_modules",
      "pnpm-lock.yaml",
      "*.tsbuildinfo",
      "**/routeTree.gen.ts",
      "apps/web/public/mockServiceWorker.js",
      "apps/web/src/lib/vendor/qrcodegen.ts",
      "*.icon/**",
    ],
    sortPackageJson: {},
    overrides: [
      {
        files: [".devcontainer/devcontainer.json"],
        options: {
          trailingComma: "none",
        },
      },
    ],
  },
  lint: {
    ignorePatterns: [
      ".repos",
      ".repos/**",
      "dist",
      "node_modules",
      "pnpm-lock.yaml",
      "*.tsbuildinfo",
      "**/routeTree.gen.ts",
    ],
    plugins: ["eslint", "oxc", "react", "unicorn", "typescript"],
    jsPlugins: ["./oxlint-plugin-bibcode/index.ts"],
    categories: {
      correctness: "warn",
      suspicious: "warn",
      perf: "warn",
    },
    rules: {
      "unicorn/no-array-sort": "off",
      "unicorn/consistent-function-scoping": "off",
      "oxc/no-map-spread": "off",
      "react-in-jsx-scope": "off",
      "react-hooks/exhaustive-deps": "off",
      "eslint/no-shadow": "off",
      "eslint/no-await-in-loop": "off",
      "eslint/no-underscore-dangle": "off",
      "typescript/consistent-return": "off",
      "typescript/no-base-to-string": "off",
      "typescript/no-duplicate-type-constituents": "off",
      "typescript/no-floating-promises": "off",
      "typescript/no-implied-eval": "off",
      "typescript/no-meaningless-void-operator": "off",
      "typescript/no-redundant-type-constituents": "off",
      "typescript/no-unnecessary-boolean-literal-compare": "off",
      "typescript/no-unnecessary-type-conversion": "off",
      "typescript/no-unnecessary-type-arguments": "off",
      "typescript/no-unnecessary-type-assertion": "off",
      "typescript/no-unnecessary-type-parameters": "off",
      "typescript/no-unsafe-type-assertion": "off",
      "typescript/await-thenable": "off",
      "typescript/require-array-sort-compare": "off",
      "typescript/restrict-template-expressions": "off",
      "typescript/unbound-method": "off",
      "eslint/no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "@bibcode/client-runtime",
              message:
                "Import from an explicit @bibcode/client-runtime/* subpath. The package has no root export.",
            },
          ],
        },
      ],
      "bibcode/no-effect-event-in-memo-or-forward-ref": "error",
      "bibcode/no-global-process-runtime": "error",
      "bibcode/no-inline-schema-compile": "warn",
      "bibcode/no-manual-effect-runtime-in-tests": "error",
      "bibcode/namespace-node-imports": "error",
    },
    options: {
      // Revisit once Oxlint's tsgolint path can integrate with @effect/tsgo diagnostics.
      typeAware: false,
      typeCheck: false,
    },
  },
});
