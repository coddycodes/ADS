#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
upstream_plugin="$script_directory/coddy-studio-linuxdeploy-gtk-upstream.sh"

if [[ ! -x "$upstream_plugin" ]]; then
  printf 'Coddy Studio AppImage packaging error: upstream GTK plugin is not executable: %s\n' \
    "$upstream_plugin" >&2
  exit 1
fi

appdir=""
appdir_seen=false
arguments=("$@")
upstream_arguments=()
for ((index = 0; index < ${#arguments[@]}; index += 1)); do
  case "${arguments[index]}" in
    --appdir)
      upstream_arguments+=("--appdir")
      appdir_seen=true
      if ((index + 1 >= ${#arguments[@]})); then
        printf 'Coddy Studio AppImage packaging error: --appdir requires a path.\n' >&2
        exit 1
      fi
      appdir="${arguments[index + 1]}"
      upstream_arguments+=("$appdir")
      index=$((index + 1))
      ;;
    --appdir=*)
      appdir_seen=true
      appdir="${arguments[index]#--appdir=}"
      upstream_arguments+=("--appdir" "$appdir")
      ;;
    *)
      upstream_arguments+=("${arguments[index]}")
      ;;
  esac
done

if [[ "$appdir_seen" == true ]]; then
  if ! gdk_pixbuf_moduledir="$(pkg-config --variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0)"; then
    printf '%s\n' \
      'Coddy Studio AppImage packaging error: cannot read GdkPixbuf build metadata. Install the Tauri Linux development prerequisites; see docs/testing/linux-desktop.md.' >&2
    exit 1
  fi
  if [[ -z "$gdk_pixbuf_moduledir" || ! -d "$gdk_pixbuf_moduledir" ]]; then
    printf 'Coddy Studio AppImage packaging error: the pinned GTK plugin requires the legacy GdkPixbuf loader directory, but this build host advertises a missing path: %s\n' \
      "${gdk_pixbuf_moduledir:-<empty>}" >&2
    printf '%s\n' \
      'Newer GdkPixbuf/Glycin layouts are not supported by this pinned bundler. Build the AppImage on the matching-architecture Ubuntu 22.04 baseline, or use the official release AppImage.' \
      'See docs/testing/linux-desktop.md. Do not create an empty loader tree, downgrade host libraries, or bypass the GTK plugin.' >&2
    exit 1
  fi
fi

"$upstream_plugin" "${upstream_arguments[@]}"

if [[ "$appdir_seen" == false ]]; then
  exit 0
fi
if [[ -z "$appdir" || ! -d "$appdir" ]]; then
  printf 'Coddy Studio AppImage packaging error: invalid AppDir: %s\n' "$appdir" >&2
  exit 1
fi

gtk_hook="$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
if [[ ! -f "$gtk_hook" ]]; then
  printf 'Coddy Studio AppImage packaging error: missing GTK AppRun hook: %s\n' \
    "$gtk_hook" >&2
  exit 1
fi

# Validate the pinned upstream hook before changing any AppDir contents.
backend_line_count="$(grep -c '^export GDK_BACKEND=x11' "$gtk_hook" || true)"
if [[ "$backend_line_count" != 1 ]]; then
  printf 'Coddy Studio AppImage packaging error: expected exactly one export GDK_BACKEND=x11 line in %s; found %s.\n' \
    "$gtk_hook" "$backend_line_count" >&2
  exit 1
fi

theme_patterns=(
  '^gsettings get org\.gnome\.desktop\.interface gtk-theme'
  '^APPIMAGE_GTK_THEME="\${APPIMAGE_GTK_THEME:-'
  '^export GTK_THEME="\$APPIMAGE_GTK_THEME"'
)
theme_labels=('gsettings gtk-theme' 'APPIMAGE_GTK_THEME default' 'GTK_THEME export')
for index in "${!theme_patterns[@]}"; do
  theme_line_count="$(grep -c "${theme_patterns[index]}" "$gtk_hook" || true)"
  if [[ "$theme_line_count" != 1 ]]; then
    printf 'Coddy Studio AppImage packaging error: expected exactly one %s line in %s; found %s.\n' \
      "${theme_labels[index]}" "$gtk_hook" "$theme_line_count" >&2
    exit 1
  fi
done

shopt -s nullglob
library_roots=("$appdir"/usr/lib*)
if ((${#library_roots[@]} > 0)); then
  find "${library_roots[@]}" \
    \( -type f -o -type l \) \
    -name 'libwayland-client.so*' \
    -delete

  remaining_library="$(
    find "${library_roots[@]}" \
      \( -type f -o -type l \) \
      -name 'libwayland-client.so*' \
      -print -quit
  )"
  if [[ -n "$remaining_library" ]]; then
    printf 'Coddy Studio AppImage packaging error: failed to remove bundled Wayland client: %s\n' \
      "$remaining_library" >&2
    exit 1
  fi
fi

# GTK_THEME pins a variant and prevents GTK/WebKitGTK following prefer-dark.
# Leave inherited GTK_THEME alone; only an explicit AppImage override replaces it.
sed -i \
  -e 's/^export GDK_BACKEND=x11.*/export GDK_BACKEND="${CODDY_STUDIO_GDK_BACKEND:-wayland,x11}"/' \
  -e '/^gsettings get org\.gnome\.desktop\.interface gtk-theme/d' \
  -e '/^APPIMAGE_GTK_THEME="\${APPIMAGE_GTK_THEME:-/d' \
  -e 's/^export GTK_THEME="\$APPIMAGE_GTK_THEME".*/if [ -n "${APPIMAGE_GTK_THEME:-}" ]; then export GTK_THEME="$APPIMAGE_GTK_THEME"; fi/' \
  "$gtk_hook"

theme_override_line='if [ -n "${APPIMAGE_GTK_THEME:-}" ]; then export GTK_THEME="$APPIMAGE_GTK_THEME"; fi'
theme_override_count="$(grep -Fxc "$theme_override_line" "$gtk_hook" || true)"
if grep -q '^[[:space:]]*export GTK_THEME=' "$gtk_hook" || [[ "$theme_override_count" != 1 ]]; then
  printf 'Coddy Studio AppImage packaging error: GTK theme rewrite failed in %s; expected only one conditional APPIMAGE_GTK_THEME override.\n' \
    "$gtk_hook" >&2
  exit 1
fi
