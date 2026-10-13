//! Process-wide allocator policy.
//!
//! On Linux, BiBCode allocates through glibc malloc, which keeps freed memory in the arena
//! that served it and returns only the free space at the top of a heap. One live chunk above
//! a large freed region, which any later allocation can be, keeps the whole region resident.
//! A full-store load or the startup validation snapshot therefore left gigabytes of free but
//! resident heap behind (#79). A background thread returns it with `malloc_trim(0)`, which
//! also releases the free pages inside every arena.

use std::time::Duration;

/// Long enough that a burst of loads reuses its heap instead of faulting it back in each
/// time; short enough that the memory returns soon after the burst. A trim with nothing to
/// release measured about 2 ms; one that released 15 GB measured about 450 ms.
const TRIM_INTERVAL: Duration = Duration::from_secs(30);

/// Starts the thread that trims the heap every [`TRIM_INTERVAL`]. Call it once per process,
/// at start, after the crash hook. On the desktop it must follow the PATH hydration, which
/// runs before any thread exists. A no-op outside Linux with glibc.
pub fn start_heap_trimmer() {
    if !cfg!(all(target_os = "linux", target_env = "gnu")) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("bibcode-heap-trim".to_owned())
        .spawn(|| {
            loop {
                std::thread::sleep(TRIM_INTERVAL);
                trim();
            }
        });
    if let Err(error) = spawned {
        // Before logging starts, so stderr is the only place this can go.
        eprintln!("bibcode: the heap trimmer did not start: {error}");
    }
}

/// Returns the free memory inside every malloc arena to the OS. A no-op outside Linux with
/// glibc.
pub fn trim() {
    // SAFETY: `malloc_trim` takes a padding size by value and locks each arena itself.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}
