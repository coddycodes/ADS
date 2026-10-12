//! Git children must not copy the server (#85). Inside the AppImage, every Git command
//! gets a PATH without the bundled directories. Rust then refuses `posix_spawn` for a
//! bare program name and forks instead, which copies the page tables of the whole
//! process: about 140 ms per command from a 6 GB host.
#![cfg(target_os = "linux")]

use std::{
    ffi::OsString,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use bibcode_server::git::{
    GitRepository, OutputPolicy, ProcessRequest, ProcessRunner, manager::refs::build_refs_snapshot,
};
use tokio_util::sync::CancellationToken;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;
#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
#[path = "support/reexec.rs"]
mod reexec;

use isolated_git_config::IsolatedGitConfig;

const SPAWN_TEST: &str = "git_runner_children_do_not_copy_the_server";
const BENCH_TEST: &str = "refs_snapshot_timing_by_host_size";
const PROBE: &str = "bibcode-spawn-probe";
const REGION: usize = 64 << 20;

/// The base page size: 4 KiB on x86_64, up to 64 KiB on some ARM64 kernels.
fn page() -> usize {
    // SAFETY: sysconf has no preconditions.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("page size")
}

/// Re-executes `test` with the environment an AppImage launch gives the server.
fn run_as_appimage(test: &str, phase: &str, configure: impl FnOnce(&mut Command)) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let appdir = fixture.path().join("AppDir");
    let probes = fixture.path().join("bin");
    fs::create_dir_all(appdir.join("usr/bin")).expect("bundled bin directory");
    fs::create_dir_all(&probes).expect("probe directory");
    executable_fixture::write_executable(&probes.join(PROBE), "#!/bin/sh\nexit 0\n");
    let mut path = vec![appdir.join("usr/bin"), probes];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("inherited PATH"),
    ));
    reexec::run(test, phase, None, |command| {
        configure(command);
        command
            .env("APPIMAGE", fixture.path().join("bibcode.AppImage"))
            .env("APPDIR", &appdir)
            .env("PATH", std::env::join_paths(path).expect("PATH"));
    });
}

fn request(command: &str, args: &[&str], cwd: &Path) -> ProcessRequest {
    ProcessRequest {
        operation: "spawn-probe".to_owned(),
        command: PathBuf::from(command),
        args: args.iter().map(OsString::from).collect(),
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
        stdin: None,
        timeout: Duration::from_secs(15),
        max_output_bytes: 8192,
        output_policy: OutputPolicy::Error,
        append_truncation_marker: false,
        allow_non_zero_exit: false,
    }
}

/// A touched anonymous region in small pages. `fork` write-protects every page of the
/// parent, so writing them again after a fork faults once per page; after `posix_spawn`
/// (which shares nothing writable) it faults on none.
struct Region(*mut u8);

impl Region {
    fn new() -> Self {
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
        // SAFETY: the range is the mapping just created. Small pages make one fault per page.
        unsafe { libc::madvise(address, REGION, libc::MADV_NOHUGEPAGE) };
        let region = Self(address.cast());
        region.write_pages();
        region
    }

    fn write_pages(&self) {
        for offset in (0..REGION).step_by(page()) {
            // SAFETY: the offset is inside the mapping, which this value owns.
            unsafe { self.0.add(offset).write_volatile(offset as u8) };
        }
    }

    /// Minor faults this thread takes to write every page again.
    fn rewrite_faults(&self) -> i64 {
        let before = minor_faults();
        self.write_pages();
        minor_faults() - before
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping created in `new`.
        unsafe { libc::munmap(self.0.cast(), REGION) };
    }
}

fn minor_faults() -> i64 {
    // SAFETY: getrusage writes only into the zeroed struct passed by pointer.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) },
        0
    );
    usage.ru_minflt
}

#[test]
fn git_runner_children_do_not_copy_the_server() {
    let Some(child) = reexec::enter(SPAWN_TEST, "appimage") else {
        run_as_appimage(SPAWN_TEST, "appimage", |_| {});
        return;
    };
    let region = Region::new();
    let pages = (REGION / page()) as i64;

    // Control: a pre-exec hook forces fork, and the probe must see it.
    let mut forked = Command::new("/bin/true");
    // SAFETY: the hook only returns; it calls nothing that is unsafe after fork.
    unsafe { forked.pre_exec(|| Ok(())) };
    assert!(forked.status().expect("forked control").success());
    let control = region.rewrite_faults();
    assert!(
        control > pages / 2,
        "the fault probe must detect a fork: {control} faults for {pages} pages"
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime")
        .block_on(async {
            let cancellation = CancellationToken::new();
            let cwd = std::env::temp_dir();
            // A bare name found through the isolated PATH, as every Git command is.
            ProcessRunner
                .run(request(PROBE, &[], &cwd), &cancellation)
                .await
                .expect("probe runs");
            ProcessRunner
                .run_bytes(request(PROBE, &[], &cwd), &cancellation)
                .await
                .expect("probe runs as bytes");
        });
    let faults = region.rewrite_faults();
    assert!(
        faults < 64,
        "a Git runner child copied the server: {faults} write faults on {pages} pages"
    );
    child.complete();
}

/// `cargo test -p bibcode-server --test linux_appimage_git_spawn -- --ignored --nocapture`
/// prints the median `build_refs_snapshot` time with an idle host and with a host that
/// holds `BIBCODE_BENCH_HEAP_GIB` (default 4) GiB of touched heap.
#[test]
#[ignore = "benchmark"]
fn refs_snapshot_timing_by_host_size() {
    if let Some(child) = reexec::enter(BENCH_TEST, "small") {
        print_refs_timing(0);
        child.complete();
        return;
    }
    if let Some(child) = reexec::enter(BENCH_TEST, "large") {
        let gib = std::env::var("BIBCODE_BENCH_HEAP_GIB")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4);
        print_refs_timing(gib);
        child.complete();
        return;
    }
    let config = IsolatedGitConfig::new();
    let repository = tempfile::tempdir().expect("repository fixture");
    create_repository(repository.path(), &config);
    let results = repository.path().join(".git/bench-results");
    for phase in ["small", "large"] {
        run_as_appimage(BENCH_TEST, phase, |command| {
            config.apply_to_command(command);
            command
                .arg("--include-ignored")
                .env("BIBCODE_BENCH_REPOSITORY", repository.path())
                .env("BIBCODE_BENCH_RESULTS", &results);
        });
    }
    print!(
        "{}",
        fs::read_to_string(results).expect("benchmark results")
    );
}

fn create_repository(path: &Path, config: &IsolatedGitConfig) {
    let git_command = || {
        let mut command = Command::new("git");
        config.apply_to_command(&mut command);
        command.current_dir(path);
        command
    };
    let git = |args: &[&str]| {
        let output = git_command().args(args).output().expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    for index in 0..200 {
        fs::write(path.join(format!("file-{index}.txt")), format!("{index}\n")).unwrap();
    }
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Bench",
        "-c",
        "user.email=bench@example.invalid",
        "commit",
        "-q",
        "-m",
        "fixture",
    ]);
    let head = git(&["rev-parse", "HEAD"]);
    // About the user's 296 refs: local branches, remote-tracking branches and tags.
    let mut updates = String::new();
    for index in 0..100 {
        updates.push_str(&format!("create refs/heads/branch-{index} {head}\n"));
        updates.push_str(&format!(
            "create refs/remotes/origin/branch-{index} {head}\n"
        ));
        updates.push_str(&format!("create refs/tags/v0.{index} {head}\n"));
    }
    let mut child = git_command()
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("update-ref");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(updates.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    git(&["remote", "add", "origin", "https://127.0.0.1:1/bench.git"]);
}

fn print_refs_timing(heap_gib: usize) {
    let mut heap: Vec<Vec<u8>> = Vec::new();
    for _ in 0..heap_gib * 1024 {
        heap.push(vec![1_u8; 1 << 20]);
    }
    let repository = PathBuf::from(
        std::env::var_os("BIBCODE_BENCH_REPOSITORY").expect("repository fixture path"),
    );
    let mut samples = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime")
        .block_on(async {
            let git = GitRepository::default();
            let cancellation = CancellationToken::new();
            let mut samples = Vec::new();
            for _ in 0..15 {
                let started = Instant::now();
                build_refs_snapshot(&git, &repository, &cancellation)
                    .await
                    .expect("refs snapshot");
                samples.push(started.elapsed());
            }
            samples
        });
    samples.sort();
    let line = format!(
        "heap={heap_gib}GiB build_refs_snapshot median={:?} min={:?} max={:?}\n",
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1],
    );
    let results = std::env::var_os("BIBCODE_BENCH_RESULTS").expect("benchmark results path");
    let mut existing = fs::read_to_string(&results).unwrap_or_default();
    existing.push_str(&line);
    fs::write(results, existing).expect("benchmark results");
    std::hint::black_box(&heap);
}
