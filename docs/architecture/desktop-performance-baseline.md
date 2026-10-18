# Desktop Performance Baseline

This document records the Windows measurements that justified replacing the
Electron shell with Tauri 2 and the final measurements of the in-process Rust
architecture. The Electron and transitional Tauri captures remain historical
migration evidence.

## Host

- Captured: 2026-07-08 through 2026-07-10
- OS: Windows 10 Pro for Workstations, 64-bit
- CPU: AMD Ryzen AI MAX+ 395 with Radeon 8060S
- RAM: 85,520,617,472 bytes
- Node: v24.12.0
- Rust and Cargo: 1.96.0

## Method

- Compare production artifacts from the same checkout.
- Use a fresh application home and the same provider settings.
- Record process private bytes and working set after 30 seconds idle.
- Record both first-window and backend-ready timing.
- Keep the local backend enabled.
- Do not compare development processes with packaged release artifacts.

The raw, immutable measurement reports are:

- [Final Tauri/Rust cold Windows x64, 30 seconds](measurements/tauri-rust-win-x64-release-cold-30s-20260711.md)
- [Final Tauri/Rust warm Windows x64, 30 seconds](measurements/tauri-rust-win-x64-release-warm-30s-20260711.md)
- [Tauri Windows x64, 30 seconds](measurements/tauri-win-x64-release-default-30s-20260709-214307.md)
- [Electron Windows x64, 30 seconds](measurements/electron-win-x64-dir-default-30s-20260709-221951.md)

Supplemental platform-specific evidence, not part of the matched Windows
performance comparison above:

- [Linux WebKitGTK resource attribution, 2026-08-11](measurements/linux-webkitgtk-resource-attribution-20260811.md)
- [Remote latency at a 250 ms round trip, 2026-10-17](measurements/remote-latency-250ms-20261017.md)

## Results

| Metric                 | Historical Electron | Transitional Tauri 2 | Final Rust cold | Final Rust warm |
| ---------------------- | ------------------- | -------------------- | --------------- | --------------- |
| First visible window   | 6.78 s              | 1.03 s               | 1.38 s          | 1.13 s          |
| Backend ready          | 6.82 s              | 5.08 s               | 4.14 s          | 3.47 s          |
| Idle private bytes     | 577.2 MiB           | 376.4 MiB            | 249.7 MiB       | 249.9 MiB       |
| Idle working set       | 751.4 MiB           | 581.1 MiB            | 461.6 MiB       | 486.3 MiB       |
| Windows NSIS installer | 211.1 MiB           | 48.4 MiB             | 8.6 MiB         | 8.6 MiB         |

In this matched sample, Tauri used 200.8 MiB less private memory (34.8%) and
170.4 MiB less working set (22.7%). Its window appeared 5.75 seconds sooner,
its backend was ready 1.74 seconds sooner, and its installer was 77.1% smaller.

The Electron process tree had five processes. Its largest processes were the
Electron main process at 195.7 MiB private memory and its Node server process at
180.0 MiB. The transitional Tauri sample had nine processes; the Rust host used
7.5 MiB private memory, while its then-present Node sidecar remained the largest
single process at 140.4 MiB. The final architecture removes that sidecar, so
these figures must not be presented as its memory baseline.

The final cold process tree had seven processes: one `bibcode-desktop` process
and six WebView2 processes, with no Node process. The Rust host, including the
in-process Axum server, used 26.0 MiB private memory. Relative to historical
Electron, final cold idle private memory is 56.7% lower and the NSIS installer
is 95.9% smaller. The warm capture briefly included the native Codex provider
probe (`powershell.exe` plus `conhost.exe`), but still contained no Node backend.

## Current Commands

Build the canonical Tauri desktop:

```powershell
vp run build:desktop
```

Build and copy a Windows NSIS installer:

```powershell
node scripts/build-desktop-artifact.ts --platform win --target nsis --arch x64 --output-dir release --verbose
```

Measure a packaged process tree:

```powershell
node scripts/measure-desktop-runtime.ts --label tauri-win-release `
  --command target/release/bibcode-desktop.exe `
  --ready-url http://127.0.0.1:3773/.well-known/bibcode/environment `
  --window-title "BiBCode" `
  --idle-ms 30000
```

`scripts/run-msvc-x64.mjs` supplies the Visual Studio x64 environment for
Cargo and Tauri on Windows. Keep desktop package scripts behind that wrapper.

## Architectural Reading

These measurements show that changing the desktop shell produced a substantial
gain without replacing React. The frontend remains React, Vite, and TypeScript
inside the operating-system WebView.

The final architecture keeps React/Vite in the operating-system WebView and
moves HTTP/WebSocket RPC, provider supervision, orchestration, persistence,
terminal, Git, filesystem, diagnostics, and relay behavior into the in-process
Rust server. It packages no Node runtime, TypeScript server, Electron APIs, or
native helper sidecars.

## Follow-Up Measurements

- Repeat 30-second cold and warm runs on macOS and Linux.
- Measure one active terminal and a large message thread.
- Add responsiveness captures for source control, terminal scrolling, and
  settings navigation.
