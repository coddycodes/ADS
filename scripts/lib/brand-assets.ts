export const BRAND_ASSET_PATHS = {
  macIconPng: "assets/prod/coddy-studio-macos-1024.png",
  linuxIconPng: "assets/prod/coddy-studio-universal-1024.png",
  macIconIcns: "assets/prod/coddy-studio-macos.icns",
  windowsIconIco: "assets/prod/coddy-studio-windows.ico",
  webFaviconIco: "assets/prod/coddy-studio-web-favicon.ico",
  webFavicon16Png: "assets/prod/coddy-studio-web-favicon-16x16.png",
  webFavicon32Png: "assets/prod/coddy-studio-web-favicon-32x32.png",
  webAppleTouchIconPng: "assets/prod/coddy-studio-web-apple-touch-180.png",
} as const;

export type WebAssetBrand = "development" | "nightly" | "production";

export const WEB_ASSET_CHANNELS = ["latest", "nightly"] as const;

export type WebAssetChannel = (typeof WEB_ASSET_CHANNELS)[number];

export function resolveWebAssetBrandForChannel(channel: WebAssetChannel): WebAssetBrand {
  if (channel === "latest") return "production";
  if (channel === "nightly") return "nightly";
  throw new Error(`Unsupported web asset channel: ${String(channel)}`);
}

export interface IconOverride {
  readonly sourceRelativePath: string;
  readonly targetRelativePath: string;
}

const WEB_ICON_TARGET_FILENAMES = {
  faviconIco: "favicon.ico",
  favicon16Png: "favicon-16x16.png",
  favicon32Png: "favicon-32x32.png",
  appleTouchIconPng: "apple-touch-icon.png",
} as const;

const WEB_ICON_SOURCE_PATHS = {
  faviconIco: BRAND_ASSET_PATHS.webFaviconIco,
  favicon16Png: BRAND_ASSET_PATHS.webFavicon16Png,
  favicon32Png: BRAND_ASSET_PATHS.webFavicon32Png,
  appleTouchIconPng: BRAND_ASSET_PATHS.webAppleTouchIconPng,
} as const satisfies Record<keyof typeof WEB_ICON_TARGET_FILENAMES, string>;

export function resolveWebIconOverrides(
  _brand: WebAssetBrand,
  targetDirectory: string,
): ReadonlyArray<IconOverride> {
  return [
    {
      sourceRelativePath: WEB_ICON_SOURCE_PATHS.faviconIco,
      targetRelativePath: `${targetDirectory}/${WEB_ICON_TARGET_FILENAMES.faviconIco}`,
    },
    {
      sourceRelativePath: WEB_ICON_SOURCE_PATHS.favicon16Png,
      targetRelativePath: `${targetDirectory}/${WEB_ICON_TARGET_FILENAMES.favicon16Png}`,
    },
    {
      sourceRelativePath: WEB_ICON_SOURCE_PATHS.favicon32Png,
      targetRelativePath: `${targetDirectory}/${WEB_ICON_TARGET_FILENAMES.favicon32Png}`,
    },
    {
      sourceRelativePath: WEB_ICON_SOURCE_PATHS.appleTouchIconPng,
      targetRelativePath: `${targetDirectory}/${WEB_ICON_TARGET_FILENAMES.appleTouchIconPng}`,
    },
  ];
}

export const DEVELOPMENT_ICON_OVERRIDES = resolveWebIconOverrides("development", "dist/client");

export const PUBLISH_ICON_OVERRIDES = resolveWebIconOverrides("production", "dist/client");
