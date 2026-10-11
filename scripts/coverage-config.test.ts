// @effect-diagnostics nodeBuiltinImport:off - Coverage policy tests inspect root config and package metadata directly.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import { describe, expect, it } from "vite-plus/test";

const EXPECTED_INCLUDE = [
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

const EXPECTED_EXCLUDE = [
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
  "apps/web/public/theme-bootstrap.js",
  "apps/web/src/lib/vendor/qrcodegen.ts",
  "apps/web/src/routeTree.gen.ts",
] as const;

const EXPECTED_PROBE_ENTRYPOINTS = [
  "vite.config.shared.ts",
  "apps/web/vite.config.app.mjs",
  "packages/client-runtime/vite.config.runtime.ts",
] as const;

const EXPECTED_VITE_SHIMS = [
  {
    shim: "vite.config.ts",
    implementationPath: "vite.config.shared.ts",
    lines: [
      'export { default } from "./vite.config.shared";',
      'export * from "./vite.config.shared";',
    ],
  },
  {
    shim: "apps/web/vite.config.ts",
    implementationPath: "apps/web/vite.config.app.mjs",
    lines: [
      "/*",
      "* tanstackRouter({ autoCodeSplitting: true,",
      "* chunkSizeWarningLimit: 1536,",
      '* const buildSourcemap: sourcemapEnv === "hidden" ? "hidden" : sourcemapEnv === "1" || sourcemapEnv === "true";',
      "*/",
      "// @ts-expect-error -- The shim intentionally re-exports an instrumentable .mjs implementation module.",
      'export { default } from "./vite.config.app.mjs";',
    ],
  },
  {
    shim: "packages/client-runtime/vite.config.ts",
    implementationPath: "packages/client-runtime/vite.config.runtime.ts",
    lines: ['export { default } from "./vite.config.runtime";'],
  },
] as const;

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const SOURCE_GLOB = "**/*.{ts,tsx,js,mjs}";
const REPOSITORY_INFRASTRUCTURE_EXCLUDE = [
  "**/.git/**",
  "**/.codegraph/**",
  "**/.superpowers/**",
  "**/.remember/**",
  "**/.pnpm-store/**",
  "**/.turbo/**",
  "**/coverage/**",
  "**/.vitest-coverage*/**",
  "**/.vite-plus/**",
  "**/node_modules/**",
  "**/dist/**",
  "**/target/**",
  "**/.repos/**",
] as const;

type CoveragePathDisposition =
  | { readonly kind: "covered" }
  | { readonly kind: "repository-subtracted"; readonly reason: string }
  | { readonly kind: "bootstrap-excluded"; readonly reason: string }
  | { readonly kind: "missing-bootstrap-exclusion"; readonly reason: string }
  | { readonly kind: "unexpectedly-excluded" }
  | { readonly kind: "unmatched" };

interface RootViteModule {
  readonly default: {
    readonly test?: {
      readonly exclude?: readonly string[];
      readonly coverage?: {
        readonly provider?: string;
        readonly include?: readonly string[];
        readonly exclude?: readonly string[];
        readonly thresholds?: Record<string, number>;
      };
    };
  };
  readonly coverageProbeEntrypoints?: readonly string[];
}

type ImportedCoverageModule = {
  readonly default: unknown;
};

let importSequence = 0;

function readRootPackageJson(): Record<string, unknown> {
  return JSON.parse(NodeFS.readFileSync(NodePath.join(REPOSITORY_ROOT, "package.json"), "utf8"));
}

function readRepositoryFile(path: string): string {
  return NodeFS.readFileSync(NodePath.join(REPOSITORY_ROOT, path), "utf8");
}

function toPosixPath(path: string): string {
  return path.replaceAll("\\", "/");
}

function matchesAny(path: string, patterns: readonly string[]): boolean {
  return patterns.some((pattern) => NodePath.matchesGlob(path, pattern));
}

function isRepositoryInfrastructurePath(path: string): boolean {
  return matchesAny(path, REPOSITORY_INFRASTRUCTURE_EXCLUDE);
}

function listRepositorySourceFiles(): string[] {
  return NodeFS.globSync(SOURCE_GLOB, {
    cwd: REPOSITORY_ROOT,
    exclude: [...REPOSITORY_INFRASTRUCTURE_EXCLUDE],
  })
    .map((path) => toPosixPath(path))
    .sort();
}

function findViteShim(path: string) {
  return EXPECTED_VITE_SHIMS.find((entry) => entry.shim === path) ?? null;
}

function findRepositorySubtractionReason(path: string): string | null {
  if (NodePath.matchesGlob(path, "apps/desktop/e2e/**")) {
    return "packaged-desktop-ui-automation";
  }
  if (NodePath.matchesGlob(path, "apps/web/visual/typography/**")) {
    return "standalone-component-capture-fixtures";
  }
  if (NodePath.matchesGlob(path, "**/*.d.ts")) {
    return "declaration";
  }
  if (
    NodePath.matchesGlob(path, "**/*.spec.{ts,tsx,js,mjs}") ||
    NodePath.matchesGlob(path, "**/*.test.{ts,tsx,js,mjs}") ||
    NodePath.matchesGlob(path, "**/test/**") ||
    NodePath.matchesGlob(path, "**/__tests__/**")
  ) {
    return "test";
  }
  if (path === "apps/web/public/mockServiceWorker.js" || path === "apps/web/src/routeTree.gen.ts") {
    return "generated";
  }
  if (path === "apps/web/src/lib/vendor/qrcodegen.ts") {
    return "vendor";
  }
  return null;
}

function findBootstrapExclusionReason(path: string): string | null {
  if (path === "apps/web/public/theme-bootstrap.js") {
    // The classic browser script is evaluated in node:vm by scripts/lib/theme-bootstrap.test.ts.
    return "vm-evaluated-browser-bootstrap";
  }
  return findViteShim(path) ? "vite-shim" : null;
}

function classifyCoveragePath(
  path: string,
  include: readonly string[],
  exclude: readonly string[],
): CoveragePathDisposition {
  if (isRepositoryInfrastructurePath(path)) {
    return { kind: "repository-subtracted", reason: "infrastructure" };
  }

  const subtractionReason = findRepositorySubtractionReason(path);
  if (subtractionReason) {
    return { kind: "repository-subtracted", reason: subtractionReason };
  }

  const bootstrapExclusionReason = findBootstrapExclusionReason(path);
  const isIncluded = matchesAny(path, include);
  const isExcluded = matchesAny(path, exclude);

  if (bootstrapExclusionReason) {
    return isExcluded
      ? { kind: "bootstrap-excluded", reason: bootstrapExclusionReason }
      : { kind: "missing-bootstrap-exclusion", reason: bootstrapExclusionReason };
  }

  if (isExcluded) {
    return { kind: "unexpectedly-excluded" };
  }

  if (isIncluded) {
    return { kind: "covered" };
  }

  return { kind: "unmatched" };
}

async function importModuleFromRepository(path: string): Promise<unknown> {
  importSequence += 1;
  const suffix = `coverage-probe=${importSequence}`;
  const moduleUrl = NodeURL.pathToFileURL(NodePath.join(REPOSITORY_ROOT, path));
  moduleUrl.search = suffix;
  return import(moduleUrl.href);
}

async function loadRootViteModule(): Promise<RootViteModule> {
  return importModuleFromRepository("vite.config.ts") as Promise<RootViteModule>;
}

function readNonEmptyLines(path: string): string[] {
  return readRepositoryFile(path)
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

describe("root coverage policy", () => {
  it("keeps standalone capture fixtures in their explicitly tested lane", async () => {
    const viteModule = await loadRootViteModule();
    expect(viteModule.default.test?.exclude).toContain("apps/web/visual/typography/**");
    expect(readRepositoryFile(".github/workflows/typography-fixtures.yml")).toContain(
      "test run --config visual/typography/vite.config.mjs",
    );
    for (const path of NodeFS.globSync("apps/web/src/**/*.{ts,tsx}", { cwd: REPOSITORY_ROOT })) {
      const source = readRepositoryFile(path);
      expect(source, path).not.toMatch(/(?:from\s*|import\s*\()\s*["'][^"']*visual\/typography/);
    }
  });
  it("excludes local Git worktrees from the root test graph", async () => {
    const viteModule = await loadRootViteModule();

    expect(viteModule.default.test?.exclude).toContain("**/.worktrees/**");
  });

  it("covers every repository source file in the live tree after subtracting known non-owned paths", async () => {
    const viteModule = await loadRootViteModule();
    const coverage = viteModule.default.test?.coverage;
    const include = coverage?.include ?? [];
    const exclude = coverage?.exclude ?? [];
    const uncoveredRepositoryFiles: string[] = [];
    const unjustifiedExcludedFiles: string[] = [];

    for (const path of listRepositorySourceFiles()) {
      const disposition = classifyCoveragePath(path, include, exclude);

      if (disposition.kind === "covered") {
        continue;
      }

      if (
        disposition.kind === "repository-subtracted" ||
        disposition.kind === "bootstrap-excluded"
      ) {
        continue;
      }

      if (disposition.kind === "missing-bootstrap-exclusion") {
        unjustifiedExcludedFiles.push(`${path} (missing ${disposition.reason} exclusion)`);
        continue;
      }

      if (disposition.kind === "unexpectedly-excluded") {
        unjustifiedExcludedFiles.push(`${path} (unexpected exclusion)`);
        continue;
      }

      uncoveredRepositoryFiles.push(path);
    }

    expect(uncoveredRepositoryFiles).toEqual([]);
    expect(unjustifiedExcludedFiles).toEqual([]);
  });

  it("rejects a future top-level source root until the coverage policy adopts it", async () => {
    const viteModule = await loadRootViteModule();
    const include = viteModule.default.test?.coverage?.include ?? [];
    const exclude = viteModule.default.test?.coverage?.exclude ?? [];
    const syntheticPath = "new-product/src/index.ts";

    expect(isRepositoryInfrastructurePath(syntheticPath)).toBe(false);
    expect(findRepositorySubtractionReason(syntheticPath)).toBeNull();
    expect(classifyCoveragePath(syntheticPath, include, exclude)).toEqual({ kind: "unmatched" });
  });

  it("requires an explicit exclusion for the VM-evaluated theme bootstrap", async () => {
    const viteModule = await loadRootViteModule();
    const include = viteModule.default.test?.coverage?.include ?? [];
    const exclude = viteModule.default.test?.coverage?.exclude ?? [];
    const bootstrapPath = "apps/web/public/theme-bootstrap.js";

    expect(findRepositorySubtractionReason(bootstrapPath)).toBeNull();
    expect(classifyCoveragePath(bootstrapPath, include, exclude)).toEqual({
      kind: "bootstrap-excluded",
      reason: "vm-evaluated-browser-bootstrap",
    });
    expect(
      classifyCoveragePath(
        bootstrapPath,
        include,
        exclude.filter((path) => path !== bootstrapPath),
      ),
    ).toEqual({
      kind: "missing-bootstrap-exclusion",
      reason: "vm-evaluated-browser-bootstrap",
    });
  });

  it("rejects an unrelated public script until the coverage policy adopts it", async () => {
    const viteModule = await loadRootViteModule();
    const include = viteModule.default.test?.coverage?.include ?? [];
    const exclude = viteModule.default.test?.coverage?.exclude ?? [];
    const syntheticPath = "apps/web/public/new-feature.js";

    expect(isRepositoryInfrastructurePath(syntheticPath)).toBe(false);
    expect(findRepositorySubtractionReason(syntheticPath)).toBeNull();
    expect(findBootstrapExclusionReason(syntheticPath)).toBeNull();
    expect(classifyCoveragePath(syntheticPath, include, exclude)).toEqual({ kind: "unmatched" });
  });

  it("uses V8 coverage with explicit owned roots and justified excludes", async () => {
    const viteModule = await loadRootViteModule();
    const coverage = viteModule.default.test?.coverage;

    expect(coverage).toBeDefined();
    expect(coverage?.provider).toBe("v8");
    expect(coverage).not.toHaveProperty("all");
    expect(coverage?.include).toEqual(EXPECTED_INCLUDE);
    expect(coverage?.exclude).toEqual(EXPECTED_EXCLUDE);
  });

  it("enforces 90% thresholds for every primary TypeScript metric", async () => {
    const viteModule = await loadRootViteModule();
    const thresholds = viteModule.default.test?.coverage?.thresholds;

    expect(thresholds).toMatchObject({
      lines: 90,
      statements: 90,
      functions: 90,
      branches: 90,
    });
  });

  it("keeps every excluded vite config shim trivial and pointed at an included implementation module", async () => {
    const viteModule = await loadRootViteModule();
    const include = viteModule.default.test?.coverage?.include ?? [];
    const exclude = viteModule.default.test?.coverage?.exclude ?? [];

    for (const entry of EXPECTED_VITE_SHIMS) {
      expect(matchesAny(entry.shim, exclude)).toBe(true);
      expect(readNonEmptyLines(entry.shim)).toEqual(entry.lines);

      expect(matchesAny(entry.implementationPath, include)).toBe(true);
    }
  });

  it("imports vite implementation modules through cache-busted module URLs", async () => {
    const viteModule = await loadRootViteModule();
    const probeEntrypoints = viteModule.coverageProbeEntrypoints ?? [];

    expect(probeEntrypoints).toEqual(EXPECTED_PROBE_ENTRYPOINTS);

    const loadedModules = await Promise.all(
      probeEntrypoints.map(async (path: string) => ({
        path,
        module: (await importModuleFromRepository(path)) as ImportedCoverageModule,
      })),
    );

    expect(loadedModules).toHaveLength(EXPECTED_PROBE_ENTRYPOINTS.length);
    expect(
      loadedModules.every(
        ({ module }: { readonly module: ImportedCoverageModule }) =>
          typeof module === "object" && module !== null && "default" in module,
      ),
    ).toBe(true);
  });

  it("publishes the repository coverage scripts", () => {
    const packageJson = readRootPackageJson();

    expect(packageJson.scripts).toMatchObject({
      "test:coverage": "vp run test:coverage:ts && vp run test:coverage:rust",
      "test:coverage:ts": "node scripts/run-local-vp.mjs test --coverage",
      "test:coverage:rust": "node scripts/check-rust-coverage.ts",
    });
  });
});
