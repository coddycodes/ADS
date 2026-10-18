use std::{
    path::{Path, PathBuf},
    process::Command,
};

const TEST_NAME: &str = "BIBCODE_TEST_REEXEC_NAME";
const PHASE: &str = "BIBCODE_TEST_REEXEC_PHASE";
const PROOF_DIRECTORY: &str = "BIBCODE_TEST_REEXEC_PROOF_DIRECTORY";

/// Proof that the selected test entered its isolated child branch.
/// Complete it only after that branch's assertions have finished.
#[must_use]
pub struct ChildPhase {
    directory: PathBuf,
    identity: String,
}

impl ChildPhase {
    pub fn complete(self) {
        std::fs::write(self.directory.join("completed"), self.identity)
            .expect("isolated test completion proof");
    }
}

/// Enter one phase of an exact-test re-execution. Separate phases allow a
/// fixture to re-execute again with further command-local environment changes.
pub fn enter(test_name: &str, phase: &str) -> Option<ChildPhase> {
    if std::env::var(TEST_NAME).as_deref() != Ok(test_name)
        || std::env::var(PHASE).as_deref() != Ok(phase)
    {
        return None;
    }
    let directory =
        PathBuf::from(std::env::var_os(PROOF_DIRECTORY).expect("isolated test proof directory"));
    let identity = format!("{test_name}/{phase}");
    std::fs::write(directory.join("entered"), &identity).expect("isolated test entry proof");
    Some(ChildPhase {
        directory,
        identity,
    })
}

/// Runs test `name` (its full path, as `cargo test` prints it) again in a process of its
/// own, where no other test runs, and returns `None` once it proved entry and completion
/// there. In that child, returns the phase to complete after the test's assertions.
///
/// For tests that share process-wide state a sibling test can change. A test that captures
/// `tracing` events with a scoped subscriber (`set_default`) is one: each callsite caches its
/// interest process-wide when it is first hit, and while one dispatcher exists, `tracing-core`
/// asks only the default of the thread that hit it. A sibling test that hits the same
/// `tracing::warn!` first on another thread, with no subscriber, so turns it off for the
/// capture, which then reads an empty log (#112). A test that starts provider-terminal
/// observer workers is another: their admission is capped process-wide.
#[allow(dead_code)] // Not every binary that includes this module calls it.
pub fn in_own_process(name: &str) -> Option<ChildPhase> {
    const PHASE: &str = "own-process";
    if let Some(child) = enter(name, PHASE) {
        return Some(child);
    }
    run(name, PHASE, None, |_| {});
    None
}

/// Re-execute one test, requiring both its entry and successful completion.
/// An alternate executable supports tests that copy their harness into an AppDir.
pub fn run(
    test_name: &str,
    phase: &str,
    executable: Option<&Path>,
    configure: impl FnOnce(&mut Command),
) {
    let directory = tempfile::tempdir().expect("isolated test proof directory");
    let executable = executable.map_or_else(
        || std::env::current_exe().expect("test executable"),
        Path::to_path_buf,
    );
    let mut command = Command::new(executable);
    command.args(["--exact", test_name, "--nocapture", "--test-threads=1"]);
    configure(&mut command);
    command
        .env(TEST_NAME, test_name)
        .env(PHASE, phase)
        .env(PROOF_DIRECTORY, directory.path());
    let output = command.output().expect("isolated test process starts");
    let diagnostics = format!(
        "{test_name}/{phase}: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.status.success(), "{diagnostics}");
    let identity = format!("{test_name}/{phase}");
    for proof in ["entered", "completed"] {
        assert_eq!(
            std::fs::read_to_string(directory.path().join(proof)).ok(),
            Some(identity.clone()),
            "isolated test must record {proof}: {diagnostics}",
        );
    }
}
