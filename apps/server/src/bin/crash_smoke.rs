//! `crash-smoke <profiler-dir>`: arms the crash monitor, then faults in a named function, so a
//! test (and CI on every OS) can check that a real native crash leaves a minidump in
//! `<profiler-dir>/crashes/` that symbolicates to `crash_here_in_named_function`.
//!
//! With `CRASH_SMOKE_MODE=arm-and-sleep` it prints `armed` once the monitor is connected and
//! sleeps instead, so a test can kill it and check that the monitor follows.

use std::{io::Write, path::PathBuf, time::Duration};

use coddy_studio_server::diagnostics::profiler::crash;

fn main() {
    if let Some(code) = crash::dispatch_crash_monitor() {
        std::process::exit(code);
    }
    crash::attach();
    let dir = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: crash-smoke <profiler-dir>"),
    );
    crash::arm_for_test(&dir);
    if std::env::var_os("CRASH_SMOKE_MODE").is_some_and(|mode| mode == "arm-and-sleep") {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "armed");
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    crash_here_in_named_function();
}

/// The frame the CI symbolication looks for.
#[inline(never)]
fn crash_here_in_named_function() {
    // A stack frame of its own: Windows x64 keeps unwind data only for functions that have
    // one, and CI walks to the caller through it. The frame and the byte below also keep
    // this body unlike the profiler's own crash-test fault, which the MSVC linker's
    // identical-code folding would otherwise merge with it and name this frame after.
    let mut frame = [0u8; 64];
    std::hint::black_box(&mut frame);
    // A write into the unmapped first page. Not to null itself: debug builds check writes
    // through null pointers and abort before the fault happens.
    // SAFETY: none. This fault is the point of the binary.
    unsafe { std::ptr::dangling_mut::<u8>().write_volatile(0x5a) }
}
