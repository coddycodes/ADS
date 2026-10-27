#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use tokio::sync::Notify;

// Share the server's re-execution harness.
#[path = "../../../server/tests/support/reexec.rs"]
pub(crate) mod reexec;

#[path = "../../../server/tests/support/hermetic_providers.rs"]
pub(crate) mod hermetic_providers;

#[cfg(target_os = "linux")]
fn appimage_test_child(test_name: &str) -> Option<reexec::ChildPhase> {
    const PHASE: &str = "desktop-appimage";
    if let Some(child) = reexec::enter(test_name, PHASE) {
        return Some(child);
    }

    let directory = tempfile::tempdir().expect("AppImage fixture directory");
    let appdir = directory.path().join(".mount_CoddyStudio");
    let host_bin = directory.path().join("host-bin");
    std::fs::create_dir_all(&host_bin).expect("host executable fixture directory");
    reexec::run(test_name, PHASE, None, |command| {
        command
            .env("APPIMAGE", directory.path().join("Coddy Studio.AppImage"))
            .env("APPDIR", &appdir)
            .env("ARGV0", "Coddy Studio.AppImage")
            .env("OWD", directory.path())
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    appdir.join("usr/bin").display(),
                    host_bin.display()
                ),
            )
            .env(
                "LD_LIBRARY_PATH",
                format!("{}:/usr/lib", appdir.join("usr/lib").display()),
            )
            .env("PYTHONHOME", appdir.join("usr"))
            .env(
                "CODDY_STUDIO_FUTURE_PATH",
                format!("{}:/opt/host/future", appdir.join("future").display()),
            )
            .env(
                "SSH_AUTH_SOCK",
                "/run/user/1000/coddy-studio-test-agent.sock",
            )
            .env("GTK_THEME", "Adwaita")
            .env("GDK_BACKEND", "x11")
            .env("PYTHONDONTWRITEBYTECODE", "1");
    });
    None
}

/// Run assertions in an isolated harness with an AppImage launcher environment.
#[cfg(target_os = "linux")]
pub(crate) fn with_appimage_test_environment(test_name: &str, body: impl FnOnce()) {
    if let Some(child) = appimage_test_child(test_name) {
        let parent_environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
        body();
        assert!(parent_environment == std::env::vars_os().collect::<BTreeMap<_, _>>());
        child.complete();
    }
}

#[cfg(target_os = "linux")]
pub(crate) async fn with_appimage_test_environment_async(
    test_name: &str,
    body: impl std::future::Future<Output = ()>,
) {
    if let Some(child) = appimage_test_child(test_name) {
        let parent_environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
        body.await;
        assert!(parent_environment == std::env::vars_os().collect::<BTreeMap<_, _>>());
        child.complete();
    }
}

/// Writes an executable test fixture (a script the test then runs) from a
/// short-lived child process, with `mode` (for example `0o755`).
///
/// Written in this process with `fs::write`, the file would be open for
/// writing here for a moment, and a child that another test forks in that
/// moment keeps the descriptor until it `exec`s; running a file that is open
/// for writing fails with ETXTBSY ("Text file busy"). Writing from a child
/// keeps the descriptor out of the test process altogether.
#[cfg(unix)]
pub(crate) fn write_executable_fixture(path: &std::path::Path, contents: &str, mode: u32) {
    use std::io::Write as _;

    let mut writer = std::process::Command::new("/bin/sh")
        .args(["-c", "cat >\"$1\" && chmod \"$2\" \"$1\"", "sh"])
        .arg(path)
        .arg(format!("{mode:o}"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("start the fixture writer");
    writer
        .stdin
        .take()
        .expect("fixture writer stdin")
        .write_all(contents.as_bytes())
        .expect("write the fixture");
    assert!(
        writer.wait().expect("fixture writer status").success(),
        "could not write {}",
        path.display()
    );
}

/// A free port for tests that bind or restart onto it after releasing it.
/// Any socket another process binds there in between makes the restart fail
/// ("Address already in use"): with port 0 the port came from the kernel's
/// ephemeral range, and portpicker's 15000-25000 is where this suite's own
/// fixtures bind. Walking 25000-32767 from a per-process offset stays below
/// the ephemeral ranges (32768 on Linux, 49152 on macOS and Windows) and above
/// portpicker. Each probe claims a candidate from a process-wide cursor,
/// offset by the process id. A port probed once in this process is not probed
/// again until the cursor wraps. Each call tries at most PORT_COUNT candidates.
pub(crate) fn free_test_port() -> u16 {
    const PORT_COUNT: u32 = 7_768;
    static NEXT_OFFSET: AtomicU32 = AtomicU32::new(0);

    let process_offset = std::process::id() % PORT_COUNT;
    (0..PORT_COUNT)
        .map(|_| {
            let offset = NEXT_OFFSET.fetch_add(1, Ordering::Relaxed) % PORT_COUNT;
            25_000 + ((process_offset + offset) % PORT_COUNT) as u16
        })
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("a free port from 25000-32767")
}

/// Runs test `name` again in a process of its own, where no other test runs,
/// and returns `None` once it proved entry and completion there. In that child,
/// returns a phase the caller must complete after running the scenario and its
/// assertions. `name` is the test's full path, as `cargo test` prints it.
///
/// For tests that restart a backend on the port it has just released. In the
/// shared test process other tests fork children, and a child keeps a copy of
/// every open socket until it `exec`s, the old listener included; a restart in
/// that window fails with "Address already in use".
pub(crate) fn isolated_scenario(name: &str) -> Option<reexec::ChildPhase> {
    const PHASE: &str = "desktop-isolated-scenario";
    if let Some(child) = reexec::enter(name, PHASE) {
        return Some(child);
    }
    reexec::run(name, PHASE, None, |_| {});
    None
}

#[derive(Debug, Default)]
pub(crate) struct FixtureEvent {
    generation: AtomicU64,
    changed: Notify,
}

impl FixtureEvent {
    pub(crate) fn checkpoint(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn publish(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }

    pub(crate) async fn wait_after(&self, checkpoint: u64) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.generation.load(Ordering::Acquire) > checkpoint {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_until_at_least(&self, expected: u64) {
        loop {
            let checkpoint = self.checkpoint();
            if checkpoint >= expected {
                return;
            }
            self.wait_after(checkpoint).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureEvent;

    #[test]
    fn free_test_port_does_not_repeat_after_skipping_a_busy_port() {
        let Some(child) = super::isolated_scenario(
            "test_support::tests::free_test_port_does_not_repeat_after_skipping_a_busy_port",
        ) else {
            return;
        };

        // The isolated process starts with a fresh cursor. Keep its first
        // candidate busy, but leave both returned ports unbound.
        let first_candidate = 25_000 + (std::process::id() % 7_768) as u16;
        let _occupied = match std::net::TcpListener::bind(("127.0.0.1", first_candidate)) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => None,
            Err(error) => panic!("bind the first candidate: {error}"),
        };

        let first = super::free_test_port();
        let second = super::free_test_port();

        assert!((25_000..=32_767).contains(&first));
        assert!((25_000..=32_767).contains(&second));
        assert_ne!(first, second);
        child.complete();
    }

    #[test]
    #[should_panic(expected = "isolated test must record entered")]
    fn isolated_scenario_rejects_a_child_that_never_entered() {
        // This test passes without entering an isolated scenario. Its successful
        // libtest result alone must not count as proof that the scenario ran.
        let _ =
            super::isolated_scenario("test_support::tests::publication_before_wait_is_observed");
    }

    #[tokio::test]
    async fn publication_before_wait_is_observed() {
        let event = FixtureEvent::default();
        let checkpoint = event.checkpoint();

        event.publish();
        event.wait_after(checkpoint).await;
    }

    #[tokio::test]
    async fn fixture_events_have_independent_generations() {
        let first = FixtureEvent::default();
        let second = FixtureEvent::default();
        let first_checkpoint = first.checkpoint();
        let second_checkpoint = second.checkpoint();

        first.publish();

        first.wait_after(first_checkpoint).await;
        assert_eq!(second.checkpoint(), second_checkpoint);
    }

    #[tokio::test]
    async fn waits_for_an_expected_generation_without_skipping_publication() {
        let event = FixtureEvent::default();

        event.publish();

        event.wait_until_at_least(1).await;
    }
}
