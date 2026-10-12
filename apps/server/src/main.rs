use std::process::ExitCode;

use bibcode_server::diagnostics::profiler::crash;

fn main() -> ExitCode {
    // The crash monitor is this binary re-executed: before the runtime, and before anything
    // else installs signal handlers or starts threads.
    if let Some(code) = crash::dispatch_crash_monitor() {
        return ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX));
    }
    crash::attach();
    bibcode_server::allocator::start_heap_trimmer();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async_main())
}

async fn async_main() -> ExitCode {
    // The open-url shim, or the Windows `bibcode-open-url.exe` alias: before clap, whose
    // environment-backed options a session may carry, and before any data-root work.
    if let Some(url) = bibcode_server::open_url::current_open_url_invocation() {
        std::process::exit(bibcode_server::open_url::run_open_url(&url).await);
    }
    match bibcode_server::run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(bibcode_server::RunError::Cli(error)) => {
            let success = error.exit_code() == 0;
            if let Err(print_error) = error.print() {
                eprintln!("bibcode: failed to print command-line help: {print_error}");
                return ExitCode::FAILURE;
            }
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("bibcode: {error}");
            ExitCode::FAILURE
        }
    }
}
