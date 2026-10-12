//! Runner children must not copy the server (#85). Inside the AppImage, every Git command
//! gets a PATH without the bundled directories. Rust then refuses `posix_spawn` for a
//! bare program name and forks instead, which copies the page tables of the whole
//! process: about 140 ms per command from a 6 GB host.
#![cfg(target_os = "linux")]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use bibcode_server::git::{
    GitRepository, OutputPolicy, ProcessRequest, ProcessRunner, manager::refs::build_refs_snapshot,
};
use bibcode_server::process::ProcessRunInput;
use tokio_util::sync::CancellationToken;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;
#[path = "support/fork_probe.rs"]
mod fork_probe;
#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
#[path = "support/reexec.rs"]
mod reexec;

use fork_probe::{NO_COPY_FAULTS, TouchedRegion};
use isolated_git_config::IsolatedGitConfig;

const GIT_RUNNER_TEST: &str = "git_runner_children_do_not_copy_the_server";
const GENERIC_RUNNER_TEST: &str = "generic_runner_children_do_not_copy_the_server";
const BENCH_TEST: &str = "refs_snapshot_timing_by_host_size";
const PROBE: &str = "bibcode-spawn-probe";

/// Re-executes `test` with the environment an AppImage launch gives the server, and a
/// bare-named probe executable on its `PATH`.
fn run_as_appimage(test: &str, phase: &str, configure: impl FnOnce(&mut Command)) {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let probes = fixture.path().join("bin");
    fs::create_dir_all(&probes).expect("probe directory");
    executable_fixture::write_executable(&probes.join(PROBE), "#!/bin/sh\nexit 0\n");
    let environment = fork_probe::appimage_environment(fixture.path(), &[probes]);
    reexec::run(test, phase, None, |command| {
        configure(command);
        command.envs(environment);
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

/// In the AppImage re-execution of `test`, runs `spawn` and fails if it copied the process.
fn assert_spawns_without_copying(test: &str, runner: &str, spawn: impl AsyncFnOnce()) {
    let Some(child) = reexec::enter(test, "appimage") else {
        run_as_appimage(test, "appimage", |_| {});
        return;
    };
    let region = TouchedRegion::new();
    region.assert_detects_a_fork();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime")
        .block_on(spawn());
    let faults = region.rewrite_faults();
    assert!(
        faults < NO_COPY_FAULTS,
        "a {runner} child copied the server: {faults} write faults on {} pages",
        region.pages()
    );
    child.complete();
}

#[test]
fn git_runner_children_do_not_copy_the_server() {
    // A bare name found through the isolated PATH, as every Git command is.
    assert_spawns_without_copying(GIT_RUNNER_TEST, "Git runner", async || {
        let cancellation = CancellationToken::new();
        let cwd = std::env::temp_dir();
        ProcessRunner
            .run(request(PROBE, &[], &cwd), &cancellation)
            .await
            .expect("probe runs");
        ProcessRunner
            .run_bytes(request(PROBE, &[], &cwd), &cancellation)
            .await
            .expect("probe runs as bytes");
    });
}

#[test]
fn generic_runner_children_do_not_copy_the_server() {
    // A bare name with a PATH of the caller's own in a cleared environment.
    assert_spawns_without_copying(GENERIC_RUNNER_TEST, "generic runner", async || {
        let mut input = ProcessRunInput::new(PROBE.to_owned(), Vec::<String>::new());
        input.cwd = Some(std::env::temp_dir());
        input.env = Some(BTreeMap::from([(
            "PATH".to_owned(),
            std::env::var("PATH").expect("isolated PATH"),
        )]));
        let output = bibcode_server::process::ProcessRunner
            .run(input)
            .await
            .expect("probe runs through the generic runner");
        assert_eq!(output.code, Some(0));
    });
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
