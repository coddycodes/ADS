# Out-of-process native crash capture, Rust panic backtraces, and offline symbolication (Tauri 2 desktop + Rust server)

Research date: 2026-10-09. Crate versions come from the crates.io API on that date. Source quotes come from the published crate tarballs (static.crates.io) and from upstream GitHub `main`.

"Verified locally" means I ran it on this machine: Linux x86_64, rustc 1.97.1 (2026-07-14), glibc toolchain, `dump_syms 2.3.9`, `minidump-stackwalk 0.27.0`. The scratch project used `crash-handler 0.8.1` and `minidumper 0.11.0`.

BiBCode context relevant to these findings (from the repo):

- The workspace `Cargo.toml` `[profile.release]` sets `strip = "symbols"`, `lto = "thin"`, `codegen-units = 1` and `panic = "unwind"`. The comment says panic=abort "terminates the macOS host" because wry catches ObjC exceptions.
- The desktop and server code use `catch_unwind` / `resume_unwind` in many places (server `git/watcher.rs`, `terminal/pty.rs`, `logging.rs`, `production/*`; desktop `preview/host.rs`, `updates.rs`).
- Locked versions: tauri 2.11.5, wry 0.55.1, webkit2gtk 2.0.2, webview2-com 0.38.2, tokio 1.53.1.
- `release.yml` ships the desktop app for macOS arm64 and x64 (dmg), Linux arm64 and x64 (AppImage), and **Windows arm64 and x64 (nsis)**.
- Desktop `main.rs` already dispatches on argv before Tauri starts: the open-url shim runs "no Tauri, no window, and the single-instance plugin never sees it". The app uses `tauri-plugin-single-instance`.
- The server `main` is `#[tokio::main] async fn main`.

---

## Q1. How do `crash-handler` and `minidumper` work, what are the current versions, maintenance status and targets, and is there an example of re-executing the same binary as the monitor?

### Takeaway

Use `crash-handler 0.8.1` (in-process signal / SEH / Mach hook) with `minidumper 0.11.0` (IPC client/server plus `minidump-writer 0.13.0`). This is the maintained, production-proven stack: Zed ships exactly this pattern by re-executing its own binary with `--crash-handler <socket>`.

I verified the whole pipeline end to end on Linux x86_64: a stripped binary crashes, a monitor child writes the minidump, `dump_syms` builds a symbol store from the unstripped build, and `minidump-stackwalk` prints function names with file:line. Pin crash-handler ≥ 0.8.1 because 0.8.0 has a macOS child-process hang that matters for BiBCode, which spawns many children. Avoid the `minidumper-child` and `sentry-minidump` wrappers for now: they pin older crash-handler versions without that fix.

### Cited Findings

**Versions and maintenance (crates.io API, 2026-10-09)**

| Crate                                                                             | Version | Updated    | Repository                              |
| --------------------------------------------------------------------------------- | ------- | ---------- | --------------------------------------- |
| `crash-handler`                                                                   | 0.8.1   | 2026-09-25 | EmbarkStudios/crash-handling            |
| `minidumper`                                                                      | 0.11.0  | 2026-07-20 | EmbarkStudios/crash-handling            |
| `crash-context`                                                                   | 0.8.0   | 2026-07-17 | now under rust-minidump/minidump-writer |
| `minidump-writer`                                                                 | 0.13.0  | 2026-07-20 | rust-minidump/minidump-writer           |
| `minidump-stackwalk`, `minidump-processor`, `breakpad-symbols`, `minidump-unwind` | 0.27.0  | 2026-08-11 | rust-minidump/rust-minidump             |
| `dump_syms`                                                                       | 2.3.9   | 2026-08-14 | mozilla/dump_syms                       |
| `sentry` / `sentry-minidump`                                                      | 0.49.3  | 2026-09-21 | getsentry/sentry-rust                   |
| `minidumper-child`                                                                | 0.6.1   | 2026-10-08 | timfish/minidumper-child                |

- None of the GitHub repos are archived. Last push dates: crash-handling 2026-09-25 (17 open issues), minidump-writer 2026-09-17, rust-minidump 2026-10-08, dump_syms 2026-10-05. — [GitHub API repos/EmbarkStudios/crash-handling](https://github.com/EmbarkStudios/crash-handling), [rust-minidump/rust-minidump](https://github.com/rust-minidump/rust-minidump), [mozilla/dump_syms](https://github.com/mozilla/dump_syms)
- Stale alternatives:
  - `breakpad-handler` 0.2.0 was last updated 2023-05-23, and `sentry-contrib-breakpad` 0.9.0 on 2023-11-15 (both EmbarkStudios/sentry-contrib-rust).
  - `crashpad` 0.1.2 was last updated 2021-09-20.
  - `crashpad-rs` 0.2.7 (2025-08-28) exists but binds Google Crashpad, which is C++. — [crates.io API](https://crates.io/crates/crashpad-rs)

**crash-handler mechanics** — [crash-handler README](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/README.md), [linux/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux/state.rs), [windows/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/windows/state.rs), [mac/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/mac/state.rs)

- Linux/Android:
  - It handles SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV and SIGTRAP with `sigaction(SA_ONSTACK | SA_SIGINFO)` and saves the previous six actions in `OLD_HANDLERS`.
  - It **interposes `pthread_create`** (`#[unsafe(no_mangle)] pub extern "C" fn pthread_create`) so that every thread, including C/C++ threads such as GTK/WebKit ones, gets an alternate signal stack for stack-overflow handling.
- Linux handler flow:
  - The user callback runs first.
  - `Handled(true)` installs the default handler and returns, so the fault re-triggers and the process dies with the original signal.
  - `Handled(false)` restores the _previously saved_ handlers and re-triggers into them.
  - For `kill()`-originated signals and SIGABRT it re-raises with `tgkill`.
  - It also detects "buggy code that saves and restores signal handlers temporarily with `signal` instead of `sigaction`" and re-installs itself.
- Windows:
  - It uses `SetUnhandledExceptionFilter`, `_set_invalid_parameter_handler`, `_set_purecall_handler` and a SIGABRT handler.
  - It adds a **vectored** handler (`AddVectoredExceptionHandler(1, …)`) that only acts on `STATUS_HEAP_CORRUPTION` (0xC0000374) and returns `EXCEPTION_CONTINUE_SEARCH` for everything else. Ordinary first-chance exceptions are therefore not intercepted.
  - `Handled(false)` chains to the previous unhandled-exception filter.
- macOS:
  - It uses **task-level Mach exception ports** (`task_swap_exception_ports`, mask including `EXC_MASK_CRASH`, behaviour `EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES`), serviced by a dedicated handler thread.
  - SIGABRT is handled by a signal handler instead, because it has no Mach equivalent.
  - README warning: the exception port "will take precedence over the signal handler", so signal-based handlers in the same process won't see these faults (it cites wasmtime#2456).
  - On a crash, `Handled(true)` replies `KERN_SUCCESS`, otherwise `KERN_FAILURE`. It then calls `detach(true)`, which restores the previous ports.
- API:
  - `CrashHandler::attach(Box<dyn CrashEvent>) -> Result<CrashHandler, Error>`, which returns `HandlerAlreadyInstalled` if one exists (one global handler).
  - `detach(self)`, which also runs on Drop.
  - `set_ptracer(Option<u32>)` (Linux).
  - `simulate_signal(u32)` (Linux) and `simulate_exception(Option<u32>)` (Windows/macOS) run the callback without a real crash.
  - Closures are wrapped with `unsafe fn make_crash_event`. The callback "is run in a compromised context": only async-signal-safe work, and no allocation from the crashing allocator. — [crash-handler lib.rs / linux.rs](https://github.com/EmbarkStudios/crash-handling/tree/main/crash-handler/src)

**minidumper mechanics** — [minidumper ipc/server.rs, ipc/client.rs, examples/diskwrite.rs](https://github.com/EmbarkStudios/crash-handling/tree/main/minidumper)

- `Server::with_name(SocketName)` / `Client::with_name(SocketName)`. `SocketName::Path(&Path)` works on every OS. `SocketName::Abstract(&str)` is Linux/Android only.
- Transport per OS:
  - Linux: `uds::UnixSeqpacketListener`.
  - Windows: AF_UNIX `SOCK_STREAM` socket, implemented in-crate.
  - macOS: AF_UNIX socket for messages plus a Mach port (`crash_context::ipc::Server::create(port_name)`) for the crash context. The socket path is limited to `sun_path` length (≈104–108 bytes).
- `ServerHandler` trait:
  - `create_minidump_file() -> (File, PathBuf)`
  - `on_minidump_created(Result<MinidumpBinary, Error>) -> LoopAction`
  - `on_message(kind: u32, Vec<u8>)`
  - optional `on_client_connected` / `on_client_disconnected`
- `Server::run(handler, &AtomicBool shutdown, stale_timeout: Option<Duration>)`.
- Client:
  - `request_dump(&CrashContext)` blocks for a `CRASH_ACK`. On macOS it sends via Mach with 2 s / 5 s timeouts.
  - `send_message(kind, buf)` should stay below 64 KiB.
  - `ping()` must be called before `request_dump` on macOS so earlier messages are flushed.
- Dump writing per OS:
  - Linux: the server uses `minidump_writer::MinidumpWriterConfig::new(pid, tid)` with the crash context, which ptrace-attaches to the crashed process.
  - Windows: `MinidumpWriter::dump_crash_context`, which uses MiniDumpWriteDump from the monitor process.
  - macOS: `MinidumpWriter::with_crash_context(..).dump()`.
- Re-exec example (`diskwrite.rs`): if `--server` is in argv, run the `Server`. Otherwise loop on `Client::with_name`, spawning `current_exe() --server` on failure. Then `CrashHandler::attach(make_crash_event(|cc| Handled(client.request_dump(cc).is_ok())))` and on Linux `handler.set_ptracer(Some(server.id()))`.

**minidump-writer target matrix** — [minidump-writer README](https://github.com/rust-minidump/minidump-writer)

| Target                                                   | Status                                                     |
| -------------------------------------------------------- | ---------------------------------------------------------- |
| x86_64 linux-gnu, linux-musl, windows-msvc, apple-darwin | ✅                                                         |
| aarch64 apple-darwin                                     | ✅                                                         |
| aarch64 linux-gnu, linux-musl                            | ⚠️ "Implemented (ie compiles), but untested"               |
| aarch64 pc-windows-msvc                                  | ⭕️ "Unimplemented, but could be implemented in the future" |

- Partial conflict on aarch64 Linux: crash-handler 0.6.3 fixed a SEGV on aarch64-linux (stack overflow in the signal handler from copying `CrashContext`) and "added testing of `aarch64-linux` to CI" (PR#96). — [crash-handler CHANGELOG](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/CHANGELOG.md), [issue #92](https://github.com/EmbarkStudios/crash-handling/issues/92)
- Verified locally: `cargo check` succeeds for crash-handler 0.8.1, minidumper 0.11.0 and minidump-writer 0.13.0 on `aarch64-pc-windows-msvc`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin` and `aarch64-unknown-linux-gnu`. My test crate failed only on its own Linux-only `set_ptracer` call, not on dependencies. crash-context and minidump-writer contain `target_arch = "aarch64"` Windows branches (`crash-context/src/windows.rs`, `minidump-writer/src/windows/ffi.rs`). The Windows `CrashEventResult::Jump` variant is x86_64-only. — local `cargo check`; [crash-context source](https://github.com/rust-minidump/minidump-writer)

**Bugs and issues that matter for BiBCode** — [crash-handler CHANGELOG](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/CHANGELOG.md), [issue #125](https://github.com/EmbarkStudios/crash-handling/issues/125), [#99](https://github.com/EmbarkStudios/crash-handling/issues/99), [#82](https://github.com/EmbarkStudios/crash-handling/issues/82), [#123](https://github.com/EmbarkStudios/crash-handling/issues/123), [#98](https://github.com/EmbarkStudios/crash-handling/issues/98)

- **#125 (fixed in 0.8.1, PR#126, 2026-09-25):** on macOS, after `CrashHandler::attach`, "any child process it spawns afterward (and their descendants) never dies when it takes a hardware fault … sits at 100% CPU forever". Task exception ports are inherited across fork/posix_spawn, and the old code replied `KERN_SUCCESS` for foreign tasks. 0.8.1 replies `KERN_FAILURE` and also fixes a send-right leak per exception. This issue was cross-referenced from zed-industries/zed#60389.
- **#99 (open):** "After calling `request_dump()` … the client connection becomes unusable … I always get a `Broken pipe`" (Linux). Only one dump per `Client` connection.
- **#82 (open):** "If you panic in the crash handler you will re-enter the crash handler, this is blocked by a mutex", so the process hangs.
- **#123 (open, 2026-07-28):** the `pthread_create` interposer is "active simply by _linking_ the crate, with no runtime opt-in and even if a `CrashHandler` is never attached". It breaks LeakSanitizer. 0.8.1 adds a nightly-only `--cfg sanitizer_compat` workaround.
- **#98 (open):** `pthread_create` symbol not found when statically linking glibc.
- Other open issues:
  - #90 "IPC unavailable under WINE"
  - #108 FreeBSD
  - #74 custom signal masks
  - #78 opt-in panic hooking (not implemented)

**Wrappers and their pins**

- `minidumper-child` 0.6.1 does "Takes the code from the minidumper diskwrite example and packages it in reusable form". It spawns `current_exe` with env var `_CRASH_REPORTER_SERVER`. Defaults: crashes dir `temp_dir()/Crashes`, server stale timeout 5000 ms, client connect timeout 3000 ms. **Its Cargo.toml pins `crash-handler = "0.7"`, `minidumper = "0.10"`.** — [minidumper-child 0.6.1 crate](https://crates.io/crates/minidumper-child)
- `sentry-minidump` 0.49.3 (enabled via `sentry = { version = "0.49.3", features = ["minidump"] }` plus `sentry::minidump::MinidumpIntegration`) depends on **`minidumper-child = "0.5"`**. Behaviour:
  - "The crash reporter re-launches the same binary … runs the minidump server, and exits"; it "never returns" from `sentry::init`.
  - Only "the first initialization that has a DSN starts the reporter".
  - Scope must be forwarded manually.
  - Linux/macOS/Windows only.
  - `sentry-rust-minidump` 0.18.1 is deprecated in its favour.
  - — [sentry-minidump docs](https://docs.rs/sentry-minidump/latest/sentry_minidump/), [sentry-rust-minidump README](https://crates.io/crates/sentry-rust-minidump)

**Production reference: Zed `crates/crashes`** — [zed crates/crashes/src/crashes.rs](https://github.com/zed-industries/zed/blob/main/crates/crashes/src/crashes.rs), [zed Cargo.toml](https://github.com/zed-industries/zed/blob/main/Cargo.toml), [Zed "Debugging Crashes"](https://github.com/zed-industries/zed/blob/main/docs/src/development/debugging-crashes.md)

- Workspace deps: `crash-handler = "0.8"`, `minidumper = "0.11"`, with `minidumper` built at `opt-level = 3`.
- Startup sequence:
  - Spawn `current_exe() --crash-handler <socket_path(pid)>`. Windows uses `CreateProcessW` with `STARTF_FORCEOFFFEEDBACK` "to prevent the busy cursor".
  - Retry `Client::with_name` every 100 ms.
  - Install the panic hook, then `CrashHandler::attach`.
  - The callback guards with a static `AtomicBool` "only request a minidump once". On macOS it suspends all other threads, then `client.ping()`, then `request_dump`.
  - `handler.set_ptracer(Some(child.id()))` on Linux.
  - A keepalive task pings every 10 s. The server's stale timeout is 60 s and its connect timeout 10 s.
- Zed's **panic hook** strips user strings from panic messages, sends a `Panic` message, and on Windows calls `CrashHandler.simulate_exception(Some(234))`. It then calls **`std::process::abort()`**, which turns every panic into a minidump plus process death.
- Zed's docs say the sidecar "inspects the editor's memory and creates a minidump in `~/Library/Logs/Zed` or `$XDG_DATA_HOME/zed/logs`". Analysis is `zstd -d <uuid>.dmp` then `minidump-stackwalk minidump.dmp` with an unstripped binary or a separate symbols file. A `<uuid>.json` sidecar carries the panic message, span and system specs.

**Verified locally (Linux x86_64) — end-to-end proof**

- Scratch binary: crash-handler 0.8.1 + minidumper 0.11.0, re-exec monitor via `current_exe() --crash-monitor <sock> <out.dmp>`, `SocketName::Path` in `temp_dir`, `set_ptracer(child)`, a callback gated by an `AtomicBool`, and a null write on a thread named `worker-1`.
- Results:
  - The monitor wrote the dump (`minidump result ok=true`). The process exited with **-11 (SIGSEGV preserved)**.
  - Dump sizes were 119 KB (and 35 KB on a run where bash job control observed a ptrace stop: the shell reported exit 147 = stopped).
  - `minidump-stackwalk --brief crash.dmp syms/` gave: `Thread 1 worker-1 (crashed)`, `0 mdtest!mdtest::crash_here_in_named_function [main.rs : 4 + 0x0]`, crash reason `SIGSEGV / SEGV_MAPERR`, address 0x0, and the decoded crashing instruction `mov dword [rdi], 0x2a`. Frame 4 was `set_alt_signal_stack_and_start [pthread_interpose.rs : 175]`, which is crash-handler's interposer.
  - Without symbols, the same frame printed as `mdtest + 0x3f0a0`.

### Inferences

- The recommended shape for BiBCode is a `crash` module that mirrors Zed: the desktop binary checks `--crash-monitor <socket> <dir>` in `main()` next to the existing open-url shim, before `bibcode_desktop_lib::run()`. That is before Tauri, GTK init, the single-instance plugin and any tokio runtime. A monitor that reached the single-instance plugin would forward argv to the running app and exit.
- The server's `#[tokio::main]` would need a plain `fn main` that dispatches the monitor mode before building the runtime. Otherwise the monitor process spins up a multi-threaded runtime for nothing.
- One global `CrashHandler` per process means desktop and the in-process server runtime share one handler. The desktop owns it, and the in-process server must not attach its own. For the standalone headless server, attach in its own `main`.
- `crash-handler = "0.8.1"` exactly (or `>=0.8.1, <0.9`) is required. Mixing `minidumper-child` (crash-handler 0.7) with a direct 0.8 dependency would put two crates exporting a `#[no_mangle] pthread_create` symbol into one link. I expect a duplicate-symbol link error or one silently winning, but did not test it.
- macOS #125 is directly disqualifying for 0.8.0 and older in BiBCode, which spawns provider CLIs, PTYs and git: crashing children would spin at 100% CPU.
- Windows arm64 is a shipped BiBCode target, yet minidump-writer marks it unimplemented. Treat Windows arm64 as "compiles, runtime unproven". Gate it behind a smoke test, or disable it there until verified.

### Gaps

- I did not run the monitor end to end on macOS, Windows, or aarch64 (any OS). Only Linux x86_64 was exercised.
- I found no Tauri- or wry-based open-source app that uses crash-handler/minidumper; GitHub code search was rate-limited. Zed (GPUI, not Tauri) is the closest production reference.
- ptrace environments: `PR_SET_PTRACER` (`set_ptracer`) only helps at Yama `ptrace_scope=1`. Scope 2/3, hardened kernels, and container seccomp/capability profiles can block the monitor's ptrace attach, so the dump fails. BiBCode's `release.yml` builds Docker images for the headless server, so check whether ptrace works inside the shipped container (it may need `--cap-add SYS_PTRACE` or a seccomp change). Flatpak/Snap were not checked.
- Signals to the monitor: a re-executed monitor shares the parent's process group and console, so Ctrl-C or SIGTERM to the headless server also hits the monitor. The monitor should ignore SIGINT/SIGTERM, or be spawned with `setpgid` / Windows `CREATE_NEW_PROCESS_GROUP`, so the parent isn't left unprotected while it shuts down gracefully. This is an inference; not tested.
- Whether Mac App Store sandboxing blocks the Mach bootstrap port registration used by `crash_context::ipc::Server::create` was not checked. BiBCode ships a notarized dmg, which is probably fine.

---

## Q2. Signal-handler conflicts: WebKitGTK/JSC, macOS Mach ports vs WKWebView, Windows WebView2 VEH

### Takeaway

On Linux, JavaScriptCore installs `SA_SIGINFO` handlers for SIGSEGV/SIGBUS (also SIGILL/SIGTRAP when registered) and **chains to the previously installed action** when it doesn't handle a fault. It uses SIGUSR2 (configurable) for thread suspend, which crash-handler doesn't touch. The two coexist if crash-handler stays installed and is never detached. Install order decides who sits on top. The real WebKit work, and most WebKit crashes, happen in separate processes (WebKitWebProcess, WebContent, msedgewebview2) that the host's handler never sees.

On macOS, crash-handler's task-level ports do not conflict with WKWebView, whose content runs out of process. On Windows, crash-handler's VEH is inert except for heap corruption, so it does not interfere with WebView2.

### Cited Findings

- WTF `Signals.cpp`, `SignalHandlers::finalize()` (non-Mach path) — [WebKit Source/WTF/wtf/threads/Signals.cpp](https://github.com/WebKit/WebKit/blob/main/Source/WTF/wtf/threads/Signals.cpp):
  - It installs `jscSignalHandler` with `action.sa_flags = SA_SIGINFO` — **no `SA_ONSTACK`** — and saves the prior action in `handlers.oldActions[...]`.
  - Signal mapping: `AccessFault` → SIGSEGV + SIGBUS, `IllegalInstruction` → SIGILL, `Usr` → SIGUSR2, `Breakpoint` → SIGTRAP.
  - If no JSC handler returns `Handled` it does `if (oldAction.sa_sigaction) { oldAction.sa_sigaction(sig, info, ucontext); return; }`, otherwise `restoreDefault()`.
  - The sa_mask comment reads: "Do not block this signal since it is used on non-Darwin systems to suspend and resume threads."
  - Handlers are installed only for signals with registered JSC handlers (`if (!handlers.numberOfHandlers[i]) continue;`).
- Darwin uses Mach exceptions (`handleSignalsWithMach()`, `useMach`) with **per-thread** registration (`registerThreadForMachExceptionHandling(Thread&)`). — [Signals.cpp](https://github.com/WebKit/WebKit/blob/main/Source/WTF/wtf/threads/Signals.cpp)
- `JSC::initialize()` calls `VMTraps::initializeSignals()` when `!Options::usePollingTraps()`, and `Wasm::prepareSignalingMemory()` when Wasm is supported. VMTraps registers an `AccessFault` (SIGSEGV) handler, and `SignalSender` calls `activateSignalHandlersFor(Signal::AccessFault)`. — [JavaScriptCore runtime/InitializeThreading.cpp](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/InitializeThreading.cpp), [runtime/VMTraps.cpp](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/VMTraps.cpp)
- `InitializeWebKit2()` calls `JSC::initialize()`. This is shared code (`Source/WebKit/Shared/WebKit2Initialize.cpp`). — [WebKit2Initialize.cpp](https://github.com/WebKit/WebKit/blob/main/Source/WebKit/Shared/WebKit2Initialize.cpp)
- The Wasm fault handler redirects faults in Wasm JIT code to a trap trampoline. It is compiled under `WEBASSEMBLY_SIGNALING_MEMORY`, and older versions used `WEBASSEMBLY_FAST_MEMORY`. — [WasmFaultSignalHandler.cpp](https://webkit.googlesource.com/WebKit/+/master/Source/JavaScriptCore/wasm/WasmFaultSignalHandler.cpp)
- Two related WebKit bugs: `JSC_SIGNAL_FOR_GC` selects a different GC suspend signal ([WebKit bug 225095](https://bugs.webkit.org/show_bug.cgi?id=225095)), and sending SIGUSR1 to a WebKitGTK process segfaults inside libjavascriptcoregtk ([WebKit bug 238594](https://bugs.webkit.org/show_bug.cgi?id=238594)). I only saw these in search snippets and did not read them in full.
- crash-handler Linux saves `OLD_HANDLERS` at attach and restores them on `Handled(false)` and on `detach`. See Q1. — [linux/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux/state.rs)
- crash-handler macOS README: exception ports take precedence over signal handlers. — [crash-handler README](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/README.md)
- crash-handler Windows VEH acts only on `STATUS_HEAP_CORRUPTION` and otherwise returns `EXCEPTION_CONTINUE_SEARCH`. The main path is the unhandled-exception filter. — [windows/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/windows/state.rs)
- No Tauri/wry/WebKitGTK report of crash-handler breaking WebKit turned up. Searches of the web and the crash-handling issue tracker for "webkit", "tauri", "WebView2" and "javascriptcore" returned nothing. — [EmbarkStudios/crash-handling issues](https://github.com/EmbarkStudios/crash-handling/issues)

### Inferences

- **Order on Linux (UI process):** in the UI process, JSC initializes when WebKit's process pool or webview is created, which happens after `main()`. This assumes `InitializeWebKit2` runs in the UI process too; that is likely given it lives in `Shared/`, but I did not confirm it at runtime.
- **If crash-handler attaches first** (early in `main`, as Zed does):
  - JSC later stacks on top and chains real crashes down to crash-handler, which is the correct outcome.
  - However, JSC's handler lacks `SA_ONSTACK`. A **stack overflow** on a thread therefore enters JSC's handler on the exhausted stack and double-faults before crash-handler runs, so that crash class is lost. This is unverified and follows from the flags.
- **If crash-handler attaches after webview creation:**
  - It sits on top with its alternate stack.
  - Benign JSC faults in the UI process (VMTraps / Wasm signaling memory) would hit crash-handler's callback first. If the callback writes a dump and returns `Handled(true)`, a benign fault becomes a fatal crash.
  - In the UI process JS normally doesn't run, so the risk is low but nonzero.
  - Attaching early is the safer default. Optionally re-check `sigaction(SIGSEGV)` after the webview exists, to log who is on top.
- **Never `detach()` on Linux after the webview exists.** `detach` restores the actions saved at attach time and silently removes JSC's handler (see Q3).
- SIGUSR2 and the configurable GC suspend signal are not in crash-handler's set, so there is no conflict. Avoid sending SIGUSR1 to the desktop process (WebKit bug 238594).
- macOS: JSC registers thread-level exception ports for its JS threads in the processes that run JS (WebContent). crash-handler uses task-level ports in the host. Thread-level ports take precedence for those threads, so they don't collide. The host's ObjC exceptions that wry catches never become Mach exceptions; uncaught ones end in `abort()` → SIGABRT → crash-handler's signal handler.

### Gaps

- No empirical test of crash-handler plus a live Tauri/WebKitGTK window (including a deliberate overflow on a GTK thread). This should be a dedicated smoke test.
- No primary source on whether WebView2's loader or runtime calls `SetUnhandledExceptionFilter` in the host process, which could override crash-handler's filter. A filter installed later would win over crash-handler's.

---

## Q3. Interaction with tokio and in-process Tauri; can the handler be installed or uninstalled at runtime (live toggle)?

### Takeaway

Technically yes: `attach`/`detach` work at any time on all three OSes, with only one handler at a time. But **do not implement the toggle with detach/attach.** Attach once at startup, keep the monitor child alive, and gate behaviour with an `AtomicBool` checked inside the callback. Disabled means return `Handled(false)`, which on Linux restores the previous handlers and re-raises, and on macOS replies `KERN_FAILURE`. tokio needs no special handling: worker threads are ordinary pthreads, so they get the alternate stack from the interposer and appear by name in the dump.

### Cited Findings

- `attach` errors with `HandlerAlreadyInstalled` if a handler exists. `detach` (and Drop) restores sigaltstack and handlers on Linux, the previous filter, IPH, purecall and abort handlers on Windows, and the previous exception ports and abort action on macOS. — [linux.rs / linux/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux/state.rs), [windows/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/windows/state.rs), [mac/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/mac/state.rs)
- `restore_handlers` restores "the previously installed or default" actions as recorded **at attach time**. — [linux/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux/state.rs)
- The macOS crash path calls `detach(true)` after the user callback and replies `KERN_SUCCESS` for `Handled(true)`, otherwise `KERN_FAILURE`. — [mac/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/mac/state.rs)
- The Linux `pthread_create` interposer is active at link time, even if the handler is never attached (issue #123). — [issue #123](https://github.com/EmbarkStudios/crash-handling/issues/123)
- Zed keeps the `CrashHandler` value alive inside a keepalive future, and the ping loop stops if the monitor dies. — [zed crashes.rs](https://github.com/zed-industries/zed/blob/main/crates/crashes/src/crashes.rs)
- The Linux `signal_handler` is effectively **one-shot**. Both non-Jump outcomes uninstall it: `Handled(true)` calls `install_default_handler(sig)`, and `Handled(false)` calls `restore_handlers()` for all six signals before re-triggering. — [linux/state.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux/state.rs)
- `CrashHandler::simulate_signal` (Linux) builds a synthetic `siginfo` plus `crash_context_getcontext` and calls `HandlerInner::handle_signal` **directly**. It does not go through `signal_handler`, so it does **not** uninstall anything. A simulated dump is therefore non-destructive. — [linux.rs](https://github.com/EmbarkStudios/crash-handling/blob/main/crash-handler/src/linux.rs)
- Open PR #118, "Make alternate signal stack teardown non-fatal", says detach-time `sigaltstack`/`munmap` failures currently panic. — [PR #118](https://github.com/EmbarkStudios/crash-handling/pull/118)
- Verified locally: a crash on a named std thread (`worker-1`) showed as `Thread 1 worker-1 (crashed)` with the full stack through `std::thread` and crash-handler's `set_alt_signal_stack_and_start`. tokio workers are the same kind of thread (`tokio-runtime-worker` by default). — local run

### Inferences

- Live-toggle design for BiBCode:
  - `static CRASH_CAPTURE_ENABLED: AtomicBool`, read with `Relaxed` in the callback. It is async-signal-safe.
  - The monitor process stays alive when capture is "disabled". It is about 1 idle process: Zed pings every 10 s.
  - Restarting the monitor after it dies means reconnecting a new `Client`. Swap the client via an `ArcSwap`/atomic pointer, not a Mutex, because the callback must not block.
- Why not detach:
  - On Linux it clobbers JSC's (or any later library's) handlers (Q2).
  - Each attach also re-mmaps the sigaltstack and swaps global state; a race between detach and a concurrent crash leaves no handler.
  - On macOS a detach while children are being spawned is harmless after 0.8.1.
- Caveat on "disabled = `Handled(false)`": because the Linux handler is one-shot, a single _benign_ fault passing through while crash-handler is on top silently disarms crash capture for the rest of the process. This would be, for example, a JSC VMTraps fault in the attach-after-webview ordering. The callback cannot re-attach from signal context. Mitigations: attach early (so JSC is on top and handles its own faults), and/or run a low-frequency watchdog that compares `sigaction(SIGSEGV)` to the expected handler and logs or re-attaches.
- PR #118 is further evidence against toggling with detach: teardown itself can currently panic.
- The callback must stay minimal (#82: a panic inside it deadlocks): no logging through `tracing`, no allocation, no locks other than crash-handler's own.
- In-process server inside Tauri: one handler covers both, since they are the same process. The server's tokio tasks crashing natively (FFI, libgit2, PTY code) are captured like any other thread.

### Gaps

- I did not measure attach/detach latency or its effect on threads created between toggles.
- I did not test crash-handler plus `tauri-plugin-single-instance` with a re-executed child, beyond reasoning that argv dispatch happens before the plugin.

---

## Q4. Can a minidump capture an in-process WebKitGTK crash? Which process actually crashes?

### Takeaway

WebKitGTK, WKWebView and WebView2 are all multi-process. JS, DOM and layout crashes happen in **WebKitWebProcess**, **WebContent** and **msedgewebview2 renderer** respectively, not in BiBCode's process. The host-side crash handler cannot capture them. The host only receives a "web process terminated / failed" notification. UI-process crashes (GTK, GDK/Wayland/EGL, WebKit UI-side code, wry, Rust) _are_ captured.

### Cited Findings

- WebKitGTK `WebKitWebView::web-process-terminated`: "This signal is emitted when the web process terminates abnormally due to `reason`." The page does not list the reason values; they live in `WebKitWebProcessTerminationReason`. — [webkit2gtk docs: web-process-terminated](https://webkitgtk.org/reference/webkit2gtk/stable/signal.WebView.web-process-terminated.html)
- Fedora ABRT reports for WebKitGTK crashes read "WebKitWebProcess killed by SIGSEGV", e.g. in `WTFCrash()` and `WebCore::EventListener&()`. — [RH bug 1252264](https://bugzilla.redhat.com/show_bug.cgi?id=1252264), [RH bug 1468263](https://bugzilla.redhat.com/show_bug.cgi?id=1468263)
- wry 0.55.1 exposes `WebViewBuilderExtDarwin::with_on_web_content_process_terminate_handler(Fn())`, which is "Available on macOS and iOS only". It wires `webViewWebContentProcessDidTerminate:`. wry has **no** GTK or WebView2 equivalent, but exposes the raw objects: `WebViewExtUnix::webview() -> webkit2gtk::WebView` and `WebViewExtWindows::webview() -> ICoreWebView2` / `controller()`. — [wry 0.55.1 src/lib.rs, src/wkwebview/navigation.rs](https://github.com/tauri-apps/wry)
- WebView2:
  - `ICoreWebView2Environment11::FailureReportFolderPath` "returns the path of the folder where minidump files are written" (since WebView2 Win32 1.0.1518.46).
  - "Normally when a single child process fails, a minidump will be generated and written to disk, then the ProcessFailed event is raised". Dumps are not guaranteed, and the folder may contain stale dumps.
  - `ProcessFailedReason::Crashed` usually produces dumps; Unresponsive and Terminated may not.
  - — [ICoreWebView2Environment11](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment11), [WebView2 process-related events](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/process-related-events)
- Tauri's Linux graphics guide lists the common UI-side failures on Linux: DMA-BUF renderer problems (NVIDIA plus Wayland), "Error 71 (Protocol error) dispatching to Wayland display", and silent crashes on resize. Workarounds are `__NV_DISABLE_EXPLICIT_SYNC=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1` and `WEBKIT_DISABLE_COMPOSITING_MODE=1`, and it points to tauri-apps/tauri#9394. — [Tauri v2: Linux Graphics Issues](https://v2.tauri.app/develop/debug/linux-graphics/)

### Inferences

- What to collect per OS:
  - Linux: connect `web-process-terminated` through `WebViewExtUnix::webview()` and record the reason and a timestamp. The WebKitWebProcess core itself goes to systemd-coredump (`coredumpctl`) on most distros.
  - macOS: use wry's terminate handler. WebContent crash reports land in `~/Library/Logs/DiagnosticReports`; I did not verify the path.
  - Windows: subscribe to `ProcessFailed` and harvest `FailureReportFolderPath` dumps. These are real minidumps, symbolicated against Microsoft's symbol server (`--symbols-url https://msdl.microsoft.com/download/symbols/`).
- The "Error 71" Wayland failure is a GDK fatal error in the UI process. I did not confirm whether GDK aborts (SIGABRT, which crash-handler catches) or calls `exit()`/`_exit()` (not catchable as a crash). Treat it as possibly invisible to the handler; log the exit status and stderr.
- GPU and DMA-BUF crashes inside WebKitGTK's UI-side compositor do run in the UI process and would be captured. Symbolicating them needs distro debug symbols for libwebkit2gtk, e.g. via debuginfod. Breakpad `.sym` files for system libraries are not produced by our CI.

### Gaps

- No primary-source statistics on how WebKitGTK crashes split between UI process and WebProcess. The ABRT evidence is anecdotal.
- Not verified: whether WebKitGTK's network and GPU processes also emit `web-process-terminated` (current WebKitGTK uses a separate NetworkProcess, and a GPUProcess in recent versions).

---

## Q5. Rust panic hook backtraces in release with `strip = "symbols"`; raw addresses and module base; non-main threads and tokio; panic=unwind

### Takeaway

With `strip = "symbols"`, `std::backtrace::Backtrace` resolves **nothing** in Rust code: every frame is `<unknown>`. `RUST_BACKTRACE=full` gives only ASLR-randomized absolute addresses, which are useless offline without each module's load base. `strip = "debuginfo"` keeps function names, and rustc's own docs say crash-reporting programs "should usually avoid `-Cstrip=symbols`".

For offline symbolication, the hook must record raw IPs plus module base and build-id. Alternatively it can ask the monitor for a minidump, which carries module list and debug IDs. Because BiBCode relies on panic=unwind and `catch_unwind`, the hook must **not** abort as Zed's does.

### Cited Findings

- Verified locally (rustc 1.97.1, Linux, release, `panic = "unwind"`, debug = 0). I panicked in a spawned thread and printed `Backtrace::force_capture()` in main:
  - `strip = "symbols"`: frames 0–5 `<unknown>`, frame 6 `__libc_start_main`, the rest `<unknown>`. With `RUST_BACKTRACE=full`: `0: 0x55c6bc8add91 - <unknown>` (absolute addresses only).
  - `strip = "debuginfo"`: function names resolve (`__rustc::rust_begin_unwind`, `core::panicking::panic_fmt`, `bt::deep_function_name::{closure#0}`, `bt::main`, …), with no file:line.
  - `strip = "none"` (debug = 0): names plus file:line for **std** frames only, because std ships with debuginfo. User frames have names but no lines.
  - A GNU build-id note was present in all three builds.
- rustc `-C strip`: "`debuginfo` … should leave backtraces mostly-intact". "`symbols` … On platforms which depend on this symbol table for backtraces … this can affect them so negatively as to make the trace incomprehensible. Programs which … even anything which wants crash-reporting, should usually avoid `-Cstrip=symbols`." — [rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html)
- Related macOS report: `strip = "debuginfo"` keeps backtraces and `"symbols"` does not. — [rust-lang/rust#135028](https://github.com/rust-lang/rust/issues/135028) (search snippet only)
- `debug = "line-tables-only"` is the "minimal amount of debug info for backtraces with filename/line number info" (Cargo MSRV 1.71). — [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)
- `std::panic::set_hook`: "The panic hook is invoked when a thread panics, but before the panic runtime is invoked. As such, the hook will run with both the aborting and unwinding runtimes." The hook is global across all threads. — [std::panic::set_hook](https://doc.rust-lang.org/std/panic/fn.set_hook.html)
- `backtrace` 0.3.76 (2025-09-26): `Frame::ip()` is available on all backends. `Frame::module_base_address()` returns `Some` **only on Windows** (`win64.rs`/`win32.rs` `Some(self.base_address)`); the `libunwind` backend used on Linux and macOS returns `None`. — [backtrace-rs src/backtrace](https://github.com/rust-lang/backtrace-rs/tree/master/src/backtrace)
- `findshlibs` 0.10.2, a module-enumeration crate, was last published 2021-11-11 and is stale. — [crates.io findshlibs](https://crates.io/crates/findshlibs)
- Windows `std::process::abort` → `abort_internal()` executes `int 0x29` (`__fastfail(FAST_FAIL_FATAL_APP_EXIT)`) on x86/x86_64, and `brk 0xF003` on aarch64/arm64ec. — [rust library/std/src/sys/pal/windows/mod.rs](https://github.com/rust-lang/rust/blob/main/library/std/src/sys/pal/windows/mod.rs)
- Zed's Windows panic path calls `CrashHandler.simulate_exception(Some(234))` before `std::process::abort()`. — [zed crashes.rs](https://github.com/zed-industries/zed/blob/main/crates/crashes/src/crashes.rs)
- tokio: a panic inside a spawned task unwinds to the task boundary and is returned as `JoinError` (`is_panic()`). — [tokio JoinError docs](https://docs.rs/tokio/latest/tokio/task/struct.JoinError.html). The repo already uses `error.is_panic()` then `resume_unwind` in `apps/server/src/production/git_vcs.rs` and `apps/desktop/src-tauri/src/ssh.rs`.

### Inferences

- **Hook design for BiBCode (non-fatal):**
  1. Chain to the previous hook so stderr and log output stay.
  2. Capture frame IPs with `backtrace::trace` (synchronized, because hooks can run concurrently on several threads; no resolve), capped at around 64 frames. A panic hook runs in a normal context, not a signal context, so allocation is fine.
  3. Map each IP to a module:
     - Linux/macOS: `libc::dladdr(ip)` gives `dli_fbase` and `dli_fname`. Precompute each module's build-id (ELF `NT_GNU_BUILD_ID`) or Mach-O `LC_UUID` once at startup, e.g. for the main exe via the `object` crate on `/proc/self/exe` or `_dyld_get_image_header`.
     - Windows: `module_base_address()` is already provided; get the PDB GUID/age from the PE debug directory.
  4. Store `(module debug_id, module name, ip - base)` tuples plus panic message, location and thread name. That is exactly the Breakpad addressing (`MODULE`/`FUNC` RVA), so the same `.sym` store symbolicates it with `breakpad-symbols`/`minidump-processor`, or a tiny tool can look up FUNC records.
  5. Let unwinding continue (`catch_unwind` and tokio `JoinHandle` keep working).
- Do **not** call `abort()` from the hook, as Zed does. With panic=unwind and BiBCode's many `catch_unwind` sites, that turns every recovered panic into a crash, which violates "reliability first".
- If a minidump is wanted for panics: the hook can call `simulate_signal`/`simulate_exception` → callback → `request_dump`. On Linux this is non-destructive (simulate_signal bypasses the one-shot `signal_handler`). Three constraints apply:
  - #99: the client is dead afterwards, so reconnect.
  - The monitor's `on_minidump_created` must return `LoopAction::Continue`. The diskwrite example and Zed return `Exit`, which would end the monitor after the first dump.
  - Rate-limit, e.g. the first panic per process, because the dump pauses the whole process under ptrace on Linux.
- Fatal panics remain uncapturable on Windows: double panic, panic in a no-unwind context, and `abort()`. `__fastfail` bypasses in-process SEH, VEH and unhandled-exception filters; only WER sees it. The hook therefore needs to send its data _before_ the runtime aborts. Since the hook runs before abort, the IP capture and IPC message above already cover this case.
- **Profile options, simplest first:**
  1. `strip = "debuginfo"` (+0 work: names in local logs).
  2. Keep `strip = "symbols"` for size but build `debug = "line-tables-only"` and produce `.sym` files in CI (Q6). Offline symbolication then gives names and lines.

### Gaps

- I did not measure the size of the real BiBCode binary under `strip="symbols"` vs `"debuginfo"`; the toy binary is not representative. Measure before choosing.
- std's `BacktraceFrame` does not expose a stable instruction-pointer accessor in the docs I checked (`frames()` is unstable). I did not re-verify this on 1.97, so prefer the `backtrace` crate.
- The `dladdr`-based IP→module mapping on macOS and Linux was not prototyped.

---

## Q6. Producing Breakpad `.sym` files in CI with `dump_syms` for stripped release binaries on all three OSes, and offline symbolication with `minidump-stackwalk`

### Takeaway

`dump_syms` needs the **unstripped** artifact. With BiBCode's `strip = "symbols"` profile, Linux CI must build unstripped and with some debuginfo, run `dump_syms`, then strip. This collides with `tauri build` doing compile and bundle in one step; use `tauri build --no-bundle` → post-process → `tauri bundle`.

- **Windows:** `-C strip` is ignored on MSVC and a PDB is always produced, so keep it and dump it.
- **macOS:** set `split-debuginfo = "packed"` plus debuginfo. rustc runs `dsymutil` _before_ its strip step, so the shipped binary stays stripped while the dSYM keeps full symbols.
- **Store layout:** `<module>/<DEBUGID>/<module>.sym`.
- **Symbolication:** `minidump-stackwalk <dmp> <symdir>` or `--symbols-url`.

### Cited Findings

**dump_syms 2.3.9 CLI** (from `src/main.rs` help strings) — [mozilla/dump_syms](https://github.com/mozilla/dump_syms)

- Inputs: ".dll, .exe, .pdb, .pd_, .so, .dbg" plus Mach-O.
- `-o/--output` and `--store DIR` ("Store output file as FILENAME.pdb/DEBUG_ID/FILENAME.sym").
- `--symbol-server "SRV*cache*url"` and `--debug-id`/`--code-id` (fetch from a server).
- `--check-cfi` ("Fail if there are no CFI data").
- `--arch` / `--list-arch` (fat Mach-O), `--inlines` ("emit INLINE and INLINE_ORIGIN directives"), `-j`, `--mapping-*` (source path remapping), `--extra-info`, `--no-generator`.
- Prebuilt releases via cargo-dist exist for `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`, with a shell installer ([dist-workspace.toml](https://github.com/mozilla/dump_syms/blob/main/dist-workspace.toml)). There is **no prebuilt for aarch64-pc-windows-msvc**, so run it on an x64 runner; .sym output is architecture-independent text. — dump_syms crate `dist-workspace.toml`
- Windows handling: given a PE, dump_syms reads `pe.debug_file_name()` and looks for the PDB next to it, falling back to a symbol server or cache. Given a PDB, it looks for a same-named `.dll/.exe` with a matching debug ID in the same directory for unwind data (`find_pe_for_pdb`). — dump_syms `src/windows/utils.rs`, `src/windows/pdb.rs`

**Linux (verified locally)**

- Build with `debug = "line-tables-only"`, `strip = "none"`. Then: `objcopy --only-keep-debug bt bt.debug`, `strip --strip-all bt`, `objcopy --add-gnu-debuglink=bt.debug bt`. Sizes: unstripped 4.59 MB, stripped 381 KB, `.debug` 4.21 MB.
- `dump_syms` on the unstripped, `.debug` and stripped files all print the **same** `MODULE Linux x86_64 A83437E178428CE331F11613797F1E660` and `INFO CODE_ID E13734A8…` (derived from the GNU build-id). Stripping does not change the debug ID.
- What each input contains:
  - **Stripped** binary: 3,195 `STACK` CFI records (from `.eh_frame`) and only 4 `PUBLIC`, no `FUNC`.
  - **`.debug`** file (only-keep-debug): `FUNC`/`PUBLIC`/`FILE` records but **no STACK CFI**, because `.eh_frame` isn't kept.
  - **Unstripped** binary: both.
- **The MODULE name is taken from the input file name** (`bt.debug`, `bt.full`). The minidump looks up `<debug_file>/<id>/<debug_file>.sym` using the shipped name. Run dump_syms on an unstripped copy that has the shipped file name, or rename.
- Store sizes: `bt.full --inlines --store` → 1.7 MB `.sym`. For the 9.9 MB crash-handler test binary, `.sym` was 939 KB. Tool binaries are about 14 MB each.
- **Full pipeline (verified locally):** stripped binary (997 KB) crashes, then `dump_syms full/mdtest --store syms`, then `minidump-stackwalk --brief crash.dmp syms` gives `mdtest!mdtest::crash_here_in_named_function [main.rs : 4 + 0x0]`.
- Build-id caveat:
  - rustc does not itself guarantee a GNU build-id. It depends on the system C compiler or linker default; gcc usually enables it, clang may not. Fix with `-C link-arg=-Wl,--build-id`.
  - Breakpad otherwise falls back to hashing the first page of `.text`, which can collide.
  - Sentry's Rust troubleshooting doc gives the same flag.
  - — [Rust internals: Emitting build-id](https://internals.rust-lang.org/t/emitting-build-id-when-linking-elf-binaries/15349), [Sentry Rust troubleshooting](https://docs.sentry.io/platforms/rust/troubleshooting), [IMPALA-4313](https://issues.apache.org/jira/browse/IMPALA-4313)

**Windows**

- rustc's `MsvcLinker::debuginfo(_strip, …)` ignores the strip argument and always passes `/DEBUG` and `/PDBALTPATH:%_PDB%`, embedding only the PDB file name, not the full path.
- History: `-Cstrip` on MSVC was changed to affect only the binary (rust-lang/rust#115120). Before 1.79, `strip=debuginfo` "unintentionally disabled the generation of `*.pdb` files on MSVC".
- — [rustc linker.rs](https://github.com/rust-lang/rust/blob/main/compiler/rustc_codegen_ssa/src/back/linker.rs), [rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html), [rust-lang/rust#115120](https://github.com/rust-lang/rust/pull/115120), [#123031](https://github.com/rust-lang/rust/pull/123031) (search summary)

**macOS**

- rustc `link.rs`: `SplitDebuginfo::Packed if sess.opts.debuginfo == DebugInfo::None => {}` (no dSYM without debuginfo). Otherwise, on Darwin it runs `dsymutil <out>`. **After** that it strips with `rust-objcopy --strip-all` for `Strip::Symbols` (executables) or `--strip-debug` for `Strip::Debuginfo`. — [rustc link.rs](https://github.com/rust-lang/rust/blob/main/compiler/rustc_codegen_ssa/src/back/link.rs)
- Cargo's default `split-debuginfo` on macOS is **`unpacked`** when debuginfo is enabled, so no dSYM is produced. Set `split-debuginfo = "packed"` explicitly. Other defaults: rustc uses `off` for ELF, and `packed` for MSVC and macOS. — [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html), [rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html)

**Tauri build sequencing**

- Tauri 2 CLI has `tauri bundle`: "Generate bundles and installers for your app (already built by `tauri build`)", with `--bundles`, `--no-sign` and `--target`. `tauri build --no-bundle`: "Skip the bundling step". — [Tauri v2 CLI reference](https://v2.tauri.app/reference/cli/)

**minidump-stackwalk 0.27.0** — [minidump-stackwalk README](https://github.com/rust-minidump/rust-minidump/tree/main/minidump-stackwalk), [breakpad-symbols lib.rs](https://github.com/rust-minidump/rust-minidump/blob/main/breakpad-symbols/src/lib.rs)

- Usage: `minidump-stackwalk [FLAGS] [OPTIONS] <minidump> [--] [symbols-path]...`.
- Each symbols path may be a `.sym` file or a directory searched with `<debug filename>/<debug identifier>/<debug filename>.sym`, where a `.pdb` debug filename has the `.pdb` replaced by `.sym`.
- Remote symbols: `--symbols-url` (Tecken protocol, "basically a static file server with a specific path format"; repeatable, tried in order), e.g. `https://msdl.microsoft.com/download/symbols/` or `https://symbols.mozilla.org/`. Related flags: `--symbols-cache` (default `temp_dir()/rust-minidump-cache`), `--symbols-tmp` (same filesystem as the cache) and `--symbols-download-timeout-secs` (default 1000).
- `--use-local-debuginfo` uses "debug information from local files referred to by the minidump, if present". It is built with `breakpad-symbols` features `debuginfo` and `http`.
- Output formats: `--human` (default), `--brief`, `--json` (documented stable schema since 0.9.6), `--pretty`, `--cyborg=out.json` (both), `--dump` (raw).
- Without symbols, CFI or PE unwind tables still give more precise stacks once `.sym` files are present.

### Inferences

- Windows (inference, not verified): with release `debug = 0` the always-emitted PDB should contain only linker public symbols (names, no lines), so use `debug = "line-tables-only"` for lines. x64 unwind data lives in the PE `.pdata`, so keep exe and pdb side by side when running dump_syms.
- **CI recipe (per platform job, before bundling):**
  1. Use a release-ci profile, or override with env: `CARGO_PROFILE_RELEASE_STRIP=none`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, and on macOS `CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed`. On Linux add `RUSTFLAGS=-Clink-arg=-Wl,--build-id` if `readelf -n` shows no build-id.
  2. `tauri build --no-bundle` (desktop) and `cargo build --release` (server).
  3. Run `dump_syms` per platform:
     - Linux: `dump_syms --inlines --store symbols target/<triple>/release/bibcode-desktop`, with the shipped file name, and likewise for the server binary. Then `strip --strip-all` in place, and optionally `objcopy --only-keep-debug` / `--add-gnu-debuglink` to archive a debuginfod-style `.debug`.
     - macOS: `dump_syms --inlines --store symbols <bin>.dSYM/Contents/Resources/DWARF/<bin>`. The binary is already stripped by rustc after dsymutil. A universal build needs `--arch` per slice.
     - Windows: `dump_syms --inlines --store symbols target\<triple>\release\bibcode-desktop.exe`, which finds the `.pdb` next to it.
  4. `tauri bundle` (signing and notarization stay as today).
  5. Upload `symbols/` and the raw PDB/dSYM/.debug as a non-public release artifact keyed by version. Optionally publish to a static Tecken-layout bucket so `--symbols-url` works.
- `--check-cfi` is a cheap CI guard that fails if a `.sym` lacks unwind data. On Linux this would have caught dumping the `.debug` file instead of the unstripped binary.
- The same `.sym` store serves panic-hook IP tuples (Q5) and minidumps.
- Size budget: expect `.sym` files of a few tens of MB for the real desktop binary with `--inlines`. This is only extrapolated from 1.7 MB for a 4.6 MB toy, so measure.
- Privacy: minidumps include thread stacks and nearby memory, which can contain prompts, tokens and paths. Zed strips user strings from panic messages. Keep dumps local by default and upload only with consent, which matches BiBCode's local-first model.

### Gaps

- The macOS path (`dump_syms` on a dSYM DWARF file) is untested here. Two things are unverified: whether `.sym` STACK records come out complete from the dSYM alone or also need the main Mach-O's `__unwind_info` (try `--check-cfi`), and how universal binaries interact.
- The Windows `dump_syms` run is untested here, including the aarch64 PDB and the stack-walking quality of minidump-processor on Windows arm64 dumps.
- I did not verify whether Tauri's macOS bundler copies an existing dSYM, or whether `tauri bundle` re-runs cargo when given `--no-bundle` output on all platforms.
- `dump_syms` behaviour for Linux `split-debuginfo = "packed"` (`.dwp`) was not tested. The `objcopy` route above is the verified one.
- Exact size numbers for BiBCode's real binaries were not measured.
