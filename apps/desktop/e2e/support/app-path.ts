// @effect-diagnostics nodeBuiltinImport:off - Packaged application paths require host path rules.
import * as NodePath from "node:path";

export type DesktopUiPlatform = "linux" | "mac" | "win";

export interface ResolveDesktopAppPathInput {
  readonly platform: DesktopUiPlatform;
  readonly environment: Readonly<Record<string, string | undefined>>;
}

export class DesktopAppPathConfigurationError extends Error {
  override readonly name = "DesktopAppPathConfigurationError";
}

const requiredConfiguredPath = (
  environment: Readonly<Record<string, string | undefined>>,
): string => {
  const configuredPath = environment.CODDY_STUDIO_E2E_APP_PATH?.trim();
  if (configuredPath === undefined || configuredPath.length === 0) {
    throw new DesktopAppPathConfigurationError(
      "CODDY_STUDIO_E2E_APP_PATH must point to an already-built packaged application.",
    );
  }
  return configuredPath;
};

export function resolveDesktopAppPath(input: ResolveDesktopAppPathInput): string {
  const configuredPath = requiredConfiguredPath(input.environment);

  switch (input.platform) {
    case "mac": {
      if (!NodePath.posix.isAbsolute(configuredPath)) {
        throw new DesktopAppPathConfigurationError(
          "CODDY_STUDIO_E2E_APP_PATH must be an absolute macOS application path.",
        );
      }
      if (/\.dmg$/i.test(configuredPath)) {
        throw new DesktopAppPathConfigurationError(
          "Copy the app from the DMG to an isolated install directory and set CODDY_STUDIO_E2E_APP_PATH to that .app bundle.",
        );
      }
      if (/\.app$/i.test(configuredPath)) {
        return NodePath.posix.join(configuredPath, "Contents", "MacOS", "coddy-studio-desktop");
      }
      if (/\.app\/Contents\/MacOS\/[^/]+$/i.test(configuredPath)) {
        return configuredPath;
      }
      throw new DesktopAppPathConfigurationError(
        "CODDY_STUDIO_E2E_APP_PATH must point to an installed .app bundle or its executable.",
      );
    }
    case "linux": {
      if (!NodePath.posix.isAbsolute(configuredPath) || !/\.AppImage$/i.test(configuredPath)) {
        throw new DesktopAppPathConfigurationError(
          "CODDY_STUDIO_E2E_APP_PATH must point to an absolute Linux AppImage.",
        );
      }
      return configuredPath;
    }
    case "win": {
      if (!NodePath.win32.isAbsolute(configuredPath) || !/\.exe$/i.test(configuredPath)) {
        throw new DesktopAppPathConfigurationError(
          "CODDY_STUDIO_E2E_APP_PATH must point to the absolute NSIS-installed executable.",
        );
      }
      return configuredPath;
    }
  }
}
