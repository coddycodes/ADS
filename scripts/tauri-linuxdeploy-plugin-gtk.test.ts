// @effect-diagnostics nodeBuiltinImport:off - This integration test executes a real shell wrapper.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const WRAPPER_SOURCE = NodePath.join(REPOSITORY_ROOT, "scripts/tauri/linuxdeploy-plugin-gtk.sh");
const UPSTREAM_FILENAME = "coddy-studio-linuxdeploy-gtk-upstream.sh";
const UPSTREAM_BACKEND_LINE =
  "export GDK_BACKEND=x11 # Crash with Wayland backend on Wayland - https://github.com/tauri-apps/tauri/issues/8541";
const BACKEND_LINE = 'export GDK_BACKEND="${CODDY_STUDIO_GDK_BACKEND:-wayland,x11}"';
const UPSTREAM_THEME_VARIANT_LINE =
  'gsettings get org.gnome.desktop.interface gtk-theme 2> /dev/null | grep -qi "dark" && GTK_THEME_VARIANT="dark" || GTK_THEME_VARIANT="light"';
const UPSTREAM_THEME_DEFAULT_LINE =
  'APPIMAGE_GTK_THEME="${APPIMAGE_GTK_THEME:-"Adwaita:$GTK_THEME_VARIANT"}" # Allow user to override theme (discouraged)';
const UPSTREAM_THEME_EXPORT_LINE =
  'export GTK_THEME="$APPIMAGE_GTK_THEME" # Custom themes are broken';
const THEME_OVERRIDE_LINE =
  'if [ -n "${APPIMAGE_GTK_THEME:-}" ]; then export GTK_THEME="$APPIMAGE_GTK_THEME"; fi';
const UPSTREAM_HOOK = `#!/usr/bin/env bash
export GTK_DATA_PREFIX="$APPDIR/usr"
${UPSTREAM_THEME_VARIANT_LINE}
${UPSTREAM_THEME_DEFAULT_LINE}
${UPSTREAM_BACKEND_LINE}

${UPSTREAM_THEME_EXPORT_LINE}
`;
const REWRITTEN_HOOK = `#!/usr/bin/env bash
export GTK_DATA_PREFIX="$APPDIR/usr"
${BACKEND_LINE}

${THEME_OVERRIDE_LINE}
`;
const temporaryDirectories: Array<string> = [];

function makeToolDirectory(): string {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-linuxdeploy-gtk-"));
  temporaryDirectories.push(directory);
  NodeFS.copyFileSync(WRAPPER_SOURCE, NodePath.join(directory, "linuxdeploy-plugin-gtk.sh"));
  NodeFS.chmodSync(NodePath.join(directory, "linuxdeploy-plugin-gtk.sh"), 0o755);
  NodeFS.mkdirSync(NodePath.join(directory, "loaders"));
  NodeFS.writeFileSync(
    NodePath.join(directory, "pkg-config"),
    '#!/bin/sh\n[ "$*" = "--variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0" ] || exit 64\nprintf "%s/loaders\\n" "$(dirname -- "$0")"\n',
    { mode: 0o755 },
  );
  return directory;
}

function runWrapper(
  directory: string,
  args: string[],
  options: NodeChildProcess.SpawnSyncOptionsWithStringEncoding = { encoding: "utf8" },
) {
  return NodeChildProcess.spawnSync(NodePath.join(directory, "linuxdeploy-plugin-gtk.sh"), args, {
    ...options,
    env: {
      ...process.env,
      ...options.env,
      PATH: `${directory}${NodePath.delimiter}${options.env?.PATH ?? process.env.PATH ?? ""}`,
    },
    timeout: 10_000,
  });
}

function writeUpstream(directory: string, source: string): void {
  const path = NodePath.join(directory, UPSTREAM_FILENAME);
  NodeFS.writeFileSync(path, source, { mode: 0o755 });
  NodeFS.chmodSync(path, 0o755);
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

// These preflight cases exit before the Linux-only GNU sed hook rewrite, so
// macOS can also exercise the real shell's error and discovery paths.
// oxlint-disable-next-line coddy/no-global-process-runtime -- Requires a Unix shell.
describe.skipIf(process.platform === "win32")("GdkPixbuf build-host preflight", () => {
  it("rejects an advertised but absent loader directory before the upstream plugin mutates the AppDir", () => {
    const directory = makeToolDirectory();
    const loaders = NodePath.join(directory, "loaders");
    NodeFS.rmdirSync(loaders);
    writeUpstream(directory, '#!/bin/sh\ntouch "$0.called"\nexit 23\n');

    const result = runWrapper(directory, ["--appdir", NodePath.join(directory, "app")]);

    expect(result.status).toBe(1);
    expect(result.stderr).toContain(loaders);
    expect(result.stderr).toContain("Ubuntu 22.04");
    expect(result.stderr).toContain("docs/testing/linux-desktop.md");
    expect(NodeFS.existsSync(NodePath.join(directory, `${UPSTREAM_FILENAME}.called`))).toBe(false);
    expect(NodeFS.existsSync(loaders)).toBe(false);
  });

  it("passes a supported loader layout to the pinned plugin without hiding its failure", () => {
    const directory = makeToolDirectory();
    writeUpstream(directory, '#!/bin/sh\ntouch "$0.called"\nexit 23\n');

    const result = runWrapper(directory, [`--appdir=${NodePath.join(directory, "app")}`]);

    expect(result.status).toBe(23);
    expect(NodeFS.existsSync(NodePath.join(directory, `${UPSTREAM_FILENAME}.called`))).toBe(true);
  });

  it("diagnoses missing GdkPixbuf metadata without manufacturing a loader directory", () => {
    const directory = makeToolDirectory();
    writeUpstream(directory, '#!/bin/sh\ntouch "$0.called"\nexit 23\n');
    NodeFS.writeFileSync(NodePath.join(directory, "pkg-config"), "#!/bin/sh\nexit 2\n");

    const result = runWrapper(directory, ["--appdir", NodePath.join(directory, "app")]);

    expect(result.status).toBe(1);
    expect(result.stderr).toContain("GdkPixbuf build metadata");
    expect(NodeFS.existsSync(NodePath.join(directory, `${UPSTREAM_FILENAME}.called`))).toBe(false);
  });

  it("preserves plugin discovery without requiring GTK development libraries", () => {
    const directory = makeToolDirectory();
    NodeFS.writeFileSync(NodePath.join(directory, "pkg-config"), "#!/bin/sh\nexit 2\n");
    writeUpstream(
      directory,
      '#!/bin/sh\n[ "$1" = "--plugin-api-version" ] || exit 91\nprintf "0\\n"\n',
    );

    const result = runWrapper(directory, ["--plugin-api-version"]);

    expect(result.status).toBe(0);
    expect(result.stdout).toBe("0\n");
  });
});

// oxlint-disable-next-line coddy/no-global-process-runtime -- This integration test runs a Linux shell wrapper.
describe.runIf(process.platform === "linux")("linuxdeploy GTK wrapper", () => {
  it.each(["separate", "equals"] as const)(
    "normalizes the %s AppDir argument form before deployment and cleans the AppDir",
    (argumentForm) => {
      const toolDirectory = makeToolDirectory();
      const appDirectory = NodePath.join(toolDirectory, "Coddy Studio.AppDir");
      const markerPath = NodePath.join(toolDirectory, "upstream-arguments.txt");
      writeUpstream(
        toolDirectory,
        `#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$@" > "$FAKE_PLUGIN_MARKER"
appdir=""
while (( $# > 0 )); do
  case "$1" in
    --appdir)
      if (( $# < 2 )); then
        printf '%s\\n' 'missing --appdir path' >&2
        exit 64
      fi
      appdir="$2"
      shift 2
      ;;
    --appdir=*)
      printf '%s\\n' 'unsupported --appdir=<path> argument' >&2
      exit 64
      ;;
    *)
      shift
      ;;
  esac
done
mkdir -p "$appdir/usr/lib/x86_64-linux-gnu"
printf 'bundled wayland' > "$appdir/usr/lib/x86_64-linux-gnu/libwayland-client.so.0"
ln -s libwayland-client.so.0 "$appdir/usr/lib/x86_64-linux-gnu/libwayland-client.so"
printf 'keep me' > "$appdir/usr/lib/x86_64-linux-gnu/libunrelated.so.1"
mkdir -p "$appdir/apprun-hooks"
cat > "$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh" <<'HOOK'
${UPSTREAM_HOOK}HOOK
`,
      );
      const appdirArguments =
        argumentForm === "separate" ? ["--appdir", appDirectory] : [`--appdir=${appDirectory}`];

      const result = runWrapper(toolDirectory, [...appdirArguments, "--output", "appimage"], {
        encoding: "utf8",
        env: { ...process.env, FAKE_PLUGIN_MARKER: markerPath },
      });

      expect(result.status, result.stderr).toBe(0);
      expect(NodeFS.readFileSync(markerPath, "utf8")).toBe(
        `--appdir\n${appDirectory}\n--output\nappimage\n`,
      );
      expect(
        NodeFS.existsSync(
          NodePath.join(appDirectory, "usr/lib/x86_64-linux-gnu/libwayland-client.so.0"),
        ),
      ).toBe(false);
      expect(
        NodeFS.existsSync(
          NodePath.join(appDirectory, "usr/lib/x86_64-linux-gnu/libwayland-client.so"),
        ),
      ).toBe(false);
      expect(
        NodeFS.readFileSync(
          NodePath.join(appDirectory, "usr/lib/x86_64-linux-gnu/libunrelated.so.1"),
          "utf8",
        ),
      ).toBe("keep me");
      const hook = NodeFS.readFileSync(
        NodePath.join(appDirectory, "apprun-hooks/linuxdeploy-plugin-gtk.sh"),
        "utf8",
      );
      expect(hook.split("\n").filter((line) => line === BACKEND_LINE)).toHaveLength(1);
      expect(hook).toBe(REWRITTEN_HOOK);
    },
  );

  it("rewrites the hook even when the AppDir has no library directories", () => {
    const toolDirectory = makeToolDirectory();
    const appDirectory = NodePath.join(toolDirectory, "Coddy Studio.AppDir");
    const hookPath = NodePath.join(appDirectory, "apprun-hooks/linuxdeploy-plugin-gtk.sh");
    NodeFS.mkdirSync(NodePath.dirname(hookPath), { recursive: true });
    NodeFS.writeFileSync(hookPath, UPSTREAM_HOOK);
    writeUpstream(toolDirectory, "#!/usr/bin/env bash\nexit 0\n");

    const result = runWrapper(toolDirectory, ["--appdir", appDirectory], { encoding: "utf8" });

    expect(result.status, result.stderr).toBe(0);
    expect(NodeFS.readFileSync(hookPath, "utf8")).toBe(REWRITTEN_HOOK);
  });

  it.each([
    { name: "no override", gtkTheme: undefined, appimageTheme: undefined, expected: "unset" },
    { name: "empty override", gtkTheme: undefined, appimageTheme: "", expected: "unset" },
    {
      name: "session override",
      gtkTheme: "Custom:dark",
      appimageTheme: undefined,
      expected: "Custom:dark",
    },
    {
      name: "empty AppImage override",
      gtkTheme: "Custom:light",
      appimageTheme: "",
      expected: "Custom:light",
    },
    {
      name: "AppImage override",
      gtkTheme: undefined,
      appimageTheme: "Adwaita:dark",
      expected: "Adwaita:dark",
    },
    {
      name: "both overrides",
      gtkTheme: "Custom:light",
      appimageTheme: "Adwaita:dark",
      expected: "Adwaita:dark",
    },
  ])(
    "honours $name without consulting the session theme",
    ({ gtkTheme, appimageTheme, expected }) => {
      const toolDirectory = makeToolDirectory();
      const appDirectory = NodePath.join(toolDirectory, "Coddy Studio.AppDir");
      const hookPath = NodePath.join(appDirectory, "apprun-hooks/linuxdeploy-plugin-gtk.sh");
      const gsettingsMarker = NodePath.join(toolDirectory, "gsettings-called");
      NodeFS.mkdirSync(NodePath.dirname(hookPath), { recursive: true });
      NodeFS.writeFileSync(hookPath, UPSTREAM_HOOK);
      NodeFS.writeFileSync(
        NodePath.join(toolDirectory, "gsettings"),
        '#!/usr/bin/env bash\nprintf called > "$GSETTINGS_MARKER"\nprintf "Adwaita-dark\\n"\n',
        { mode: 0o755 },
      );
      writeUpstream(toolDirectory, "#!/usr/bin/env bash\nexit 0\n");
      const rewrite = runWrapper(toolDirectory, ["--appdir", appDirectory], { encoding: "utf8" });
      expect(rewrite.status, rewrite.stderr).toBe(0);

      for (const backend of [undefined, "x11"]) {
        const result = NodeChildProcess.spawnSync(
          "bash",
          [
            "-c",
            'source "$1"; printf "%s\\n%s\\n" "${GTK_THEME-unset}" "$GDK_BACKEND"',
            "hook-test",
            hookPath,
          ],
          {
            encoding: "utf8",
            env: {
              ...process.env,
              PATH: `${toolDirectory}:${process.env.PATH}`,
              APPDIR: appDirectory,
              GTK_THEME: gtkTheme,
              APPIMAGE_GTK_THEME: appimageTheme,
              GSETTINGS_MARKER: gsettingsMarker,
              CODDY_STUDIO_GDK_BACKEND: backend,
            },
          },
        );
        expect(result.status, result.stderr).toBe(0);
        expect(result.stdout).toBe(`${expected}\n${backend ?? "wayland,x11"}\n`);
        expect(NodeFS.existsSync(gsettingsMarker)).toBe(false);
      }
    },
  );

  it.each([
    { name: "missing", hook: undefined, diagnostic: "missing GTK AppRun hook" },
    {
      name: "without the X11 export",
      hook: UPSTREAM_HOOK.replace(UPSTREAM_BACKEND_LINE, "# No backend override"),
      diagnostic: "found 0",
    },
    {
      name: "with duplicate X11 exports",
      hook: `${UPSTREAM_HOOK}${UPSTREAM_BACKEND_LINE}\n`,
      diagnostic: "found 2",
    },
    ...[
      { line: UPSTREAM_THEME_VARIANT_LINE, label: "gsettings gtk-theme" },
      { line: UPSTREAM_THEME_DEFAULT_LINE, label: "APPIMAGE_GTK_THEME default" },
      { line: UPSTREAM_THEME_EXPORT_LINE, label: "GTK_THEME export" },
    ].flatMap(({ line, label }) => [
      {
        name: `without the ${label}`,
        hook: UPSTREAM_HOOK.replace(`${line}\n`, ""),
        diagnostic: `expected exactly one ${label} line`,
      },
      {
        name: `with duplicate ${label} lines`,
        hook: `${UPSTREAM_HOOK}${line}\n`,
        diagnostic: `expected exactly one ${label} line`,
      },
    ]),
  ])("rejects a hook $name without mutating the AppDir", ({ hook, diagnostic }) => {
    const toolDirectory = makeToolDirectory();
    const appDirectory = NodePath.join(toolDirectory, "Coddy Studio.AppDir");
    const hookPath = NodePath.join(appDirectory, "apprun-hooks/linuxdeploy-plugin-gtk.sh");
    const libraryDirectory = NodePath.join(appDirectory, "usr/lib");
    NodeFS.mkdirSync(libraryDirectory, { recursive: true });
    NodeFS.writeFileSync(NodePath.join(libraryDirectory, "libwayland-client.so.0"), "preexisting");
    NodeFS.symlinkSync(
      "libwayland-client.so.0",
      NodePath.join(libraryDirectory, "libwayland-client.so"),
    );
    NodeFS.writeFileSync(NodePath.join(libraryDirectory, "libunrelated.so.1"), "keep me");
    NodeFS.mkdirSync(NodePath.dirname(hookPath), { recursive: true });
    if (hook !== undefined) NodeFS.writeFileSync(hookPath, hook);
    writeUpstream(toolDirectory, "#!/usr/bin/env bash\nexit 0\n");
    const entriesBefore = NodeFS.readdirSync(appDirectory, { recursive: true }).sort();

    const result = runWrapper(toolDirectory, ["--appdir", appDirectory], { encoding: "utf8" });

    expect(result.status, result.stderr).toBe(1);
    expect(result.stderr).toContain("Coddy Studio AppImage packaging error");
    expect(result.stderr).toContain(diagnostic);
    expect(result.stderr).toContain(hookPath);
    expect(NodeFS.readdirSync(appDirectory, { recursive: true }).sort()).toEqual(entriesBefore);
    expect(
      NodeFS.readFileSync(NodePath.join(libraryDirectory, "libwayland-client.so.0"), "utf8"),
    ).toBe("preexisting");
    expect(NodeFS.readlinkSync(NodePath.join(libraryDirectory, "libwayland-client.so"))).toBe(
      "libwayland-client.so.0",
    );
    expect(NodeFS.readFileSync(NodePath.join(libraryDirectory, "libunrelated.so.1"), "utf8")).toBe(
      "keep me",
    );
    if (hook !== undefined) expect(NodeFS.readFileSync(hookPath, "utf8")).toBe(hook);
  });

  it("preserves plugin discovery output when no AppDir argument is present", () => {
    const toolDirectory = makeToolDirectory();
    writeUpstream(
      toolDirectory,
      `#!/usr/bin/env bash
set -euo pipefail
if [[ "\${1:-}" == "--plugin-api-version" ]]; then
  printf '0\\n'
  exit 0
fi
exit 91
`,
    );

    const result = runWrapper(toolDirectory, ["--plugin-api-version"], { encoding: "utf8" });

    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toBe("0\n");
  });

  it("propagates an upstream failure without mutating the AppDir", () => {
    const toolDirectory = makeToolDirectory();
    const appDirectory = NodePath.join(toolDirectory, "Coddy Studio.AppDir");
    const libraryPath = NodePath.join(appDirectory, "usr/lib/libwayland-client.so.0");
    NodeFS.mkdirSync(NodePath.dirname(libraryPath), { recursive: true });
    NodeFS.writeFileSync(libraryPath, "preexisting");
    writeUpstream(toolDirectory, "#!/usr/bin/env bash\nexit 23\n");

    const result = runWrapper(toolDirectory, [`--appdir=${appDirectory}`], { encoding: "utf8" });

    expect(result.status).toBe(23);
    expect(NodeFS.readFileSync(libraryPath, "utf8")).toBe("preexisting");
  });
});
