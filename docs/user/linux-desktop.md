# Linux Desktop

Download the `.AppImage` matching your architecture (ARM64 or x64) from
[GitHub Releases](https://github.com/mubeda/CoddyStudio/releases), make it executable,
and launch it. See [Provider setup](../providers/README.md) to connect a coding
agent.

## Wayland and fractional scaling

The AppImage prefers native Wayland on Wayland sessions and falls back to X11
when Wayland is unavailable. X11-only desktops continue to work automatically.
This fixes oversized rendering on Hyprland/Omarchy with fractional scaling,
where forcing the app through Xwayland could render it at 2× on a 1.5× desktop.

If native Wayland causes problems on your desktop, close Coddy Studio and launch the
AppImage with the previous X11 backend:

```sh
CODDY_STUDIO_GDK_BACKEND=x11 /path/to/Coddy Studio.AppImage
```

On a Wayland session this uses Xwayland and may bring back the fractional-scaling
issue. Launch without `CODDY_STUDIO_GDK_BACKEND` to restore automatic backend
selection; remove the variable from your environment or launcher if you saved it
there.
