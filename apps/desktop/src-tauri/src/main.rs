#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The crash monitor is this binary re-executed: no Tauri, no window, and the
    // single-instance plugin never sees it.
    if let Some(code) = coddy_studio_server::diagnostics::profiler::crash::dispatch_crash_monitor()
    {
        std::process::exit(code);
    }
    // Once, before WebKit installs its signal handlers, so they chain real crashes down to it.
    coddy_studio_server::diagnostics::profiler::crash::attach();
    // The open-url shim, or the Windows `coddy-studio-open-url.exe` alias: no Tauri, no window,
    // and the single-instance plugin never sees it.
    if let Some(url) = coddy_studio_server::open_url::current_open_url_invocation() {
        attach_parent_console();
        let code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("open-url runtime")
            .block_on(coddy_studio_server::open_url::run_open_url(&url));
        std::process::exit(code);
    }
    // `<desktop exe> profiler …`: the desktop install ships no separate `coddy-studio`, so the
    // profiler CLI runs here, with no Tauri, no window and no single-instance check.
    if let Some(code) =
        coddy_studio_server::diagnostics::profiler::cli::desktop_dispatch(attach_parent_console)
    {
        std::process::exit(code);
    }
    coddy_studio_desktop_lib::run();
}

/// A release build is a GUI-subsystem executable with no console of its own, so open-url's
/// printed fallback and errors and the profiler CLI's output would go nowhere. Borrow the
/// calling shell's console when there is one; a debug build already has its own, and then
/// this fails harmlessly.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole takes a process id by value and touches no caller memory.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
