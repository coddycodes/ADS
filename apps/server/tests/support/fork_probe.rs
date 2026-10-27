//! Detects whether a child spawn copied this process (#85). A fork write-protects every
//! private page of the parent, so writing a touched region again after a fork faults once
//! per page; after `posix_spawn`, which shares nothing writable, it faults on none. Run it
//! in an isolated re-execution: a fork on another test thread would count too.
#![allow(dead_code)]

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

const REGION: usize = 64 << 20;

/// Fewer write faults than this after a spawn means it did not copy the process.
pub const NO_COPY_FAULTS: i64 = 64;

/// The base page size: 4 KiB on x86_64, up to 64 KiB on some ARM64 kernels.
fn page() -> usize {
    // SAFETY: sysconf has no preconditions.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("page size")
}

/// A touched anonymous region in base-size pages.
pub struct TouchedRegion(*mut u8);

impl TouchedRegion {
    pub fn new() -> Self {
        // SAFETY: an anonymous private mapping with no address hint; checked below.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                REGION,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED, "test region");
        // SAFETY: the range is the mapping just created. Base pages fault once per page.
        unsafe { libc::madvise(address, REGION, libc::MADV_NOHUGEPAGE) };
        let region = Self(address.cast());
        region.write_pages();
        region
    }

    pub fn pages(&self) -> i64 {
        (REGION / page()) as i64
    }

    fn write_pages(&self) {
        for offset in (0..REGION).step_by(page()) {
            // SAFETY: the offset is inside the mapping, which this value owns.
            unsafe { self.0.add(offset).write_volatile(offset as u8) };
        }
    }

    /// Minor faults this thread takes to write every page again.
    pub fn rewrite_faults(&self) -> i64 {
        let before = minor_faults();
        self.write_pages();
        minor_faults() - before
    }

    /// Fails unless the probe sees a fork: a pre-exec hook forces one.
    pub fn assert_detects_a_fork(&self) {
        use std::os::unix::process::CommandExt;

        let mut forked = std::process::Command::new("/bin/true");
        // SAFETY: the hook only returns; it calls nothing that is unsafe after fork.
        unsafe { forked.pre_exec(|| Ok(())) };
        assert!(forked.status().expect("forked control").success());
        let faults = self.rewrite_faults();
        assert!(
            faults > self.pages() / 2,
            "the fault probe must detect a fork: {faults} faults for {} pages",
            self.pages()
        );
    }
}

impl Drop for TouchedRegion {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping created in `new`.
        unsafe { libc::munmap(self.0.cast(), REGION) };
    }
}

fn minor_faults() -> i64 {
    // SAFETY: getrusage writes only into the zeroed struct passed by pointer.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_THREAD and a valid out pointer.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) },
        0
    );
    usage.ru_minflt
}

/// The variables an AppImage launch gives the server, under `root`: `APPIMAGE`, `APPDIR`,
/// and a `PATH` that starts with the bundled `bin` directory, then `extra`, then this
/// process's `PATH`. Isolation strips the bundled entry, so every isolated child gets a
/// `PATH` of its own.
pub fn appimage_environment(root: &Path, extra: &[PathBuf]) -> Vec<(&'static str, OsString)> {
    let appdir = root.join("AppDir");
    std::fs::create_dir_all(appdir.join("usr/bin")).expect("bundled bin directory");
    let mut path = vec![appdir.join("usr/bin")];
    path.extend(extra.iter().cloned());
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("inherited PATH"),
    ));
    vec![
        (
            "APPIMAGE",
            root.join("coddy-studio.AppImage").into_os_string(),
        ),
        ("APPDIR", appdir.into_os_string()),
        ("PATH", std::env::join_paths(path).expect("PATH")),
    ]
}
