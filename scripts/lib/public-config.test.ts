// @effect-diagnostics nodeBuiltinImport:off - Tests exercise root env file precedence directly.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { loadRepoEnv, resolvePublicConfig } from "./public-config.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe("loadRepoEnv", () => {
  it("does not project cloud configuration for an unconfigured clone", () => {
    const env = loadRepoEnv({ baseEnv: {}, repoRoot: makeTemporaryDirectory() });

    expect(env.CODDY_STUDIO_CLERK_PUBLISHABLE_KEY).toBeUndefined();
    expect(env.CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID).toBeUndefined();
    expect(env.VITE_CLERK_PUBLISHABLE_KEY).toBeUndefined();
    expect(env.CODDY_STUDIO_CLERK_JWT_TEMPLATE).toBeUndefined();
    expect(env.VITE_CLERK_JWT_TEMPLATE).toBeUndefined();
    expect(env.CODDY_STUDIO_RELAY_URL).toBeUndefined();
    expect(env.VITE_CODDY_STUDIO_RELAY_URL).toBeUndefined();
  });

  it("applies process, root local, and root precedence in that order", () => {
    const repoRoot = makeTemporaryDirectory();
    NodeFS.writeFileSync(
      NodePath.join(repoRoot, ".env"),
      "CODDY_STUDIO_CLERK_PUBLISHABLE_KEY=pk_root\nCODDY_STUDIO_CLERK_JWT_TEMPLATE=template_root\nCODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID=oauth_root\nCODDY_STUDIO_RELAY_URL=https://root.example.test\n",
    );
    NodeFS.writeFileSync(
      NodePath.join(repoRoot, ".env.local"),
      "CODDY_STUDIO_CLERK_PUBLISHABLE_KEY=pk_local\nCODDY_STUDIO_CLERK_JWT_TEMPLATE=template_local\nCODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID=oauth_local\nCODDY_STUDIO_RELAY_URL=https://local.example.test\n",
    );

    expect(loadRepoEnv({ baseEnv: {}, repoRoot }).CODDY_STUDIO_RELAY_URL).toBe(
      "https://local.example.test",
    );
    expect(
      loadRepoEnv({
        baseEnv: {
          CODDY_STUDIO_CLERK_PUBLISHABLE_KEY: "pk_ci",
          CODDY_STUDIO_CLERK_JWT_TEMPLATE: "template_ci",
          CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID: "oauth_ci",
          CODDY_STUDIO_RELAY_URL: "https://ci.example.test",
        },
        repoRoot,
      }),
    ).toMatchObject({
      CODDY_STUDIO_CLERK_PUBLISHABLE_KEY: "pk_ci",
      CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID: "oauth_ci",
      VITE_CLERK_PUBLISHABLE_KEY: "pk_ci",
      CODDY_STUDIO_CLERK_JWT_TEMPLATE: "template_ci",
      VITE_CLERK_JWT_TEMPLATE: "template_ci",
      CODDY_STUDIO_RELAY_URL: "https://ci.example.test",
      VITE_CODDY_STUDIO_RELAY_URL: "https://ci.example.test",
    });
  });

  it("accepts legacy framework aliases as root overrides", () => {
    expect(
      resolvePublicConfig({
        VITE_CLERK_PUBLISHABLE_KEY: "pk_legacy",
        VITE_CLERK_JWT_TEMPLATE: "template_legacy",
        CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID: "oauth_canonical",
        VITE_CODDY_STUDIO_RELAY_URL: "https://legacy.example.test",
      }),
    ).toEqual({
      clerkPublishableKey: "pk_legacy",
      clerkJwtTemplate: "template_legacy",
      clerkCliOAuthClientId: "oauth_canonical",
      relayUrl: "https://legacy.example.test",
    });
  });

  it("trims values, skips empty aliases, and preserves unrelated environment entries", () => {
    expect(
      loadRepoEnv({
        baseEnv: {
          UNRELATED: "kept",
          CODDY_STUDIO_CLERK_PUBLISHABLE_KEY: "   ",
          VITE_CLERK_PUBLISHABLE_KEY: " pk_trimmed ",
        },
        repoRoot: makeTemporaryDirectory(),
      }),
    ).toMatchObject({
      UNRELATED: "kept",
      CODDY_STUDIO_CLERK_PUBLISHABLE_KEY: "pk_trimmed",
      VITE_CLERK_PUBLISHABLE_KEY: "pk_trimmed",
    });
  });

  it("surfaces environment file read errors", () => {
    const repoRoot = makeTemporaryDirectory();
    NodeFS.mkdirSync(NodePath.join(repoRoot, ".env"));

    expect(() => loadRepoEnv({ baseEnv: {}, repoRoot })).toThrow();
  });
});

function makeTemporaryDirectory() {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-public-config-"));
  temporaryDirectories.push(directory);
  return directory;
}
