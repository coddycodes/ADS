import { defineConfig } from "vite-plus";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import * as NodePath from "node:path";

const fixturePortsPath = NodePath.resolve(import.meta.dirname, "ports.ts");
const sourceRoot = NodePath.resolve(import.meta.dirname, "../../src");
const settingsPortModules = new Set(
  [
    "state/server.ts",
    "state/environments.ts",
    "state/shell.ts",
    "state/preview.ts",
    "state/auth.ts",
    "environments/primary/sessionState.ts",
    "state/gitManager.ts",
    "state/pullRequests.ts",
    "state/vcs.ts",
    "state/assets.ts",
    "state/session.ts",
    "state/projects.ts",
    "state/review.ts",
    "state/sourceControl.ts",
    "state/threads.ts",
    "state/orchestration.ts",
  ].map((path) => NodePath.join(sourceRoot, path)),
);
const fixturePorts = {
  name: "typography-owned-service-ports",
  enforce: "pre",
  async resolveId(source, importer) {
    if (!importer || /visual\/typography\/(?:ports|.*-ports)\.[tj]sx?$/.test(importer)) return null;
    const resolved = await this.resolve(source, importer, { skipSelf: true });
    return resolved && settingsPortModules.has(resolved.id) ? fixturePortsPath : null;
  },
};

export default defineConfig({
  root: import.meta.dirname,
  plugins: [
    fixturePorts,
    react(),
    babel({ parserOpts: { plugins: ["typescript", "jsx"] }, presets: [reactCompilerPreset()] }),
    tailwindcss(),
  ],
  resolve: {
    alias: { "~": NodePath.resolve(import.meta.dirname, "../../src") },
    dedupe: ["react", "react-dom"],
  },
  build: { outDir: "../../dist/typography", emptyOutDir: true },
  test: { include: ["*.test.{ts,tsx}"], environment: "node" },
});
