// @effect-diagnostics nodeBuiltinImport:off - Build bootstrap reads optional root env files before an Effect runtime exists.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

import { readCoddyStudioEnvironmentVariable } from "@coddy/shared/environmentIdentity";

export interface CoddyStudioPublicConfig {
  readonly clerkPublishableKey: string | undefined;
  readonly clerkJwtTemplate: string | undefined;
  readonly clerkCliOAuthClientId: string | undefined;
  readonly relayUrl: string | undefined;
}

type Environment = Readonly<Record<string, string | undefined>>;

const REPO_ROOT = NodePath.dirname(
  NodePath.dirname(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url))),
);

const COMPATIBLE_ENVIRONMENT_SUFFIXES = [
  "APPLE_TEAM_ID",
  "AUTO_BOOTSTRAP_PROJECT_FROM_CWD",
  "BITBUCKET_ACCESS_TOKEN",
  "BITBUCKET_API_BASE_URL",
  "BITBUCKET_API_TOKEN",
  "BITBUCKET_EMAIL",
  "CLERK_CLI_OAUTH_CLIENT_ID",
  "CLERK_JWT_TEMPLATE",
  "CLERK_PASSKEY_RP_DOMAINS",
  "CLERK_PUBLISHABLE_KEY",
  "CLOUDFLARED_PATH",
  "DEV_INSTANCE",
  "HOME",
  "HOST",
  "LOG",
  "LOG_WS_EVENTS",
  "MACOS_PROVISIONING_PROFILE",
  "MODE",
  "NO_BROWSER",
  "PORT",
  "PORT_OFFSET",
  "RELAY_URL",
  "TAURI_DESKTOP_ALLOW_CROSS_PLATFORM",
  "TAURI_DESKTOP_ARCH",
  "TAURI_DESKTOP_OUTPUT_DIR",
  "TAURI_DESKTOP_PLATFORM",
  "TAURI_DESKTOP_SKIP_BUILD",
  "TAURI_DESKTOP_TARGET",
  "TAURI_DESKTOP_UPDATER",
  "TAURI_DESKTOP_VERBOSE",
  "WEB_SOURCEMAP",
  "WSL_SERVER_BINARY",
] as const;

export function loadRepoEnv({
  baseEnv = process.env,
  repoRoot = REPO_ROOT,
}: {
  readonly baseEnv?: Environment;
  readonly repoRoot?: string;
} = {}): Record<string, string | undefined> {
  const rootEnv = readEnvFile(NodePath.join(repoRoot, ".env"));
  const localEnv = readEnvFile(NodePath.join(repoRoot, ".env.local"));
  const config = resolvePublicConfig(baseEnv, localEnv, rootEnv);
  const env: Record<string, string | undefined> = {
    ...rootEnv,
    ...localEnv,
    ...baseEnv,
    ...(config.clerkPublishableKey
      ? {
          CODDY_STUDIO_CLERK_PUBLISHABLE_KEY: config.clerkPublishableKey,
          VITE_CLERK_PUBLISHABLE_KEY: config.clerkPublishableKey,
        }
      : {}),
    ...(config.clerkJwtTemplate
      ? {
          CODDY_STUDIO_CLERK_JWT_TEMPLATE: config.clerkJwtTemplate,
          VITE_CLERK_JWT_TEMPLATE: config.clerkJwtTemplate,
        }
      : {}),
    ...(config.clerkCliOAuthClientId
      ? {
          CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID: config.clerkCliOAuthClientId,
        }
      : {}),
    ...(config.relayUrl
      ? {
          CODDY_STUDIO_RELAY_URL: config.relayUrl,
          VITE_CODDY_STUDIO_RELAY_URL: config.relayUrl,
        }
      : {}),
  };
  for (const suffix of COMPATIBLE_ENVIRONMENT_SUFFIXES) {
    env[`CODDY_STUDIO_${suffix}`] = readCoddyStudioEnvironmentVariable(env, suffix);
  }
  return env;
}

export function resolvePublicConfig(...sources: readonly Environment[]): CoddyStudioPublicConfig {
  return {
    clerkPublishableKey: firstNonEmpty(
      sources,
      "CODDY_STUDIO_CLERK_PUBLISHABLE_KEY",
      "VITE_CLERK_PUBLISHABLE_KEY",
    ),
    clerkJwtTemplate: firstNonEmpty(
      sources,
      "CODDY_STUDIO_CLERK_JWT_TEMPLATE",
      "VITE_CLERK_JWT_TEMPLATE",
    ),
    clerkCliOAuthClientId: firstNonEmpty(sources, "CODDY_STUDIO_CLERK_CLI_OAUTH_CLIENT_ID"),
    relayUrl: firstNonEmpty(sources, "CODDY_STUDIO_RELAY_URL", "VITE_CODDY_STUDIO_RELAY_URL"),
  };
}

function firstNonEmpty(sources: readonly Environment[], ...names: readonly string[]) {
  for (const source of sources) {
    for (const name of names) {
      const value = source[name]?.trim();
      if (value) {
        return value;
      }
    }
  }
  return undefined;
}

function readEnvFile(path: string): Record<string, string | undefined> {
  return NodeFS.existsSync(path) ? NodeUtil.parseEnv(NodeFS.readFileSync(path, "utf8")) : {};
}
