const REPO = "mubeda/CoddyStudio";

export const RELEASES_URL = `https://github.com/${REPO}/releases`;

const API_URL = `https://api.github.com/repos/${REPO}/releases/latest`;
const CACHE_KEY = "coddy-studio-latest-release";

export interface ReleaseAsset {
  name: string;
  browser_download_url: string;
}

export interface Release {
  tag_name: string;
  html_url: string;
  assets: ReleaseAsset[];
}

const isInternalReleaseAsset = (name: string): boolean =>
  name.endsWith(".sig") ||
  name.endsWith(".minisig") ||
  name.endsWith(".sbom.json") ||
  name === "coddy-studio-server-SHA256SUMS" ||
  name === "latest.json" ||
  name.startsWith("updater-") ||
  // Breakpad symbols, which share the server archives' `-<os>-<arch>.zip` suffix.
  /^coddy-studio-(?:desktop|server)-symbols-/.test(name);

export function findReleaseAsset(
  assets: ReadonlyArray<ReleaseAsset>,
  suffix: string,
): ReleaseAsset | undefined {
  return assets.find((asset) => !isInternalReleaseAsset(asset.name) && asset.name.endsWith(suffix));
}

export async function fetchLatestRelease(): Promise<Release> {
  const cached = sessionStorage.getItem(CACHE_KEY);
  if (cached) return JSON.parse(cached);

  const data = await fetch(API_URL).then((r) => r.json());

  if (data?.assets) {
    sessionStorage.setItem(CACHE_KEY, JSON.stringify(data));
  }

  return data;
}
