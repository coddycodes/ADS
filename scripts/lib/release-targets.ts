export type ReleasePlatform = "mac" | "linux" | "win";
export type ReleaseArch = "arm64" | "x64";
export type ServerArchiveKind = "tar.gz" | "zip";

export const TAURI_UPDATE_TARGETS = [
  "darwin-aarch64",
  "darwin-x86_64",
  "linux-aarch64",
  "linux-x86_64",
  "windows-aarch64",
  "windows-x86_64",
] as const;

export type TauriUpdaterTarget = (typeof TAURI_UPDATE_TARGETS)[number];

export interface ReleaseTarget {
  readonly platform: ReleasePlatform;
  readonly arch: ReleaseArch;
  readonly runner: string;
  readonly rustTarget: string;
  readonly desktopBundle: "dmg" | "appimage" | "nsis";
  readonly updaterTarget: TauriUpdaterTarget;
  readonly serverArchive: ServerArchiveKind;
  readonly serverOs: "darwin" | "linux" | "windows";
  readonly serverArch: "aarch64" | "x86_64";
  readonly debArch?: "amd64" | "arm64";
  readonly rpmArch?: "x86_64" | "aarch64";
}

export const RELEASE_TARGETS = [
  {
    platform: "mac",
    arch: "arm64",
    runner: "macos-26",
    rustTarget: "aarch64-apple-darwin",
    desktopBundle: "dmg",
    updaterTarget: "darwin-aarch64",
    serverArchive: "tar.gz",
    serverOs: "darwin",
    serverArch: "aarch64",
  },
  {
    platform: "mac",
    arch: "x64",
    runner: "macos-15-intel",
    rustTarget: "x86_64-apple-darwin",
    desktopBundle: "dmg",
    updaterTarget: "darwin-x86_64",
    serverArchive: "tar.gz",
    serverOs: "darwin",
    serverArch: "x86_64",
  },
  {
    platform: "linux",
    arch: "arm64",
    runner: "ubuntu-22.04-arm",
    rustTarget: "aarch64-unknown-linux-gnu",
    desktopBundle: "appimage",
    updaterTarget: "linux-aarch64",
    serverArchive: "tar.gz",
    serverOs: "linux",
    serverArch: "aarch64",
    debArch: "arm64",
    rpmArch: "aarch64",
  },
  {
    platform: "linux",
    arch: "x64",
    runner: "ubuntu-22.04",
    rustTarget: "x86_64-unknown-linux-gnu",
    desktopBundle: "appimage",
    updaterTarget: "linux-x86_64",
    serverArchive: "tar.gz",
    serverOs: "linux",
    serverArch: "x86_64",
    debArch: "amd64",
    rpmArch: "x86_64",
  },
  {
    platform: "win",
    arch: "arm64",
    runner: "windows-11-vs2026-arm",
    rustTarget: "aarch64-pc-windows-msvc",
    desktopBundle: "nsis",
    updaterTarget: "windows-aarch64",
    serverArchive: "zip",
    serverOs: "windows",
    serverArch: "aarch64",
  },
  {
    platform: "win",
    arch: "x64",
    runner: "windows-2025",
    rustTarget: "x86_64-pc-windows-msvc",
    desktopBundle: "nsis",
    updaterTarget: "windows-x86_64",
    serverArchive: "zip",
    serverOs: "windows",
    serverArch: "x86_64",
  },
] as const satisfies ReadonlyArray<ReleaseTarget>;

/**
 * The release asset with the Breakpad symbols of one target's desktop or server binary. It uses
 * the public OS/architecture keys of the server archives.
 */
export function symbolArchiveName(
  family: "desktop" | "server",
  version: string,
  target: ReleaseTarget,
): string {
  return `coddy-studio-${family}-symbols-${version}-${target.serverOs}-${target.serverArch}.zip`;
}

export function findReleaseTarget(
  platform: ReleasePlatform,
  arch: ReleaseArch,
): ReleaseTarget | undefined {
  return RELEASE_TARGETS.find((target) => target.platform === platform && target.arch === arch);
}

export function requireReleaseTarget(platform: ReleasePlatform, arch: ReleaseArch): ReleaseTarget {
  const target = findReleaseTarget(platform, arch);
  if (target === undefined) {
    throw new Error(`Unsupported release target ${platform}/${arch}.`);
  }
  return target;
}
