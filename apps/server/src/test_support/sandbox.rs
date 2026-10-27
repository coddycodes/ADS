use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use std::{
    ffi::OsStr,
    io::Read,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::process::ProcessRunInput;

#[derive(Debug)]
pub(crate) struct TestSandbox {
    root: TempDir,
    environment: BTreeMap<OsString, OsString>,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

#[derive(Debug)]
pub(crate) struct FixtureLease {
    active: Arc<AtomicUsize>,
}

impl TestSandbox {
    pub(crate) fn new(name: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("coddy-studio-server-{name}-"))
            .tempdir()
            .expect("test sandbox temporary root");
        Self {
            root,
            environment: std::env::vars_os()
                .filter(|(name, _)| !super::isolated_git_config::is_git_environment_variable(name))
                .collect(),
            active: Arc::new(AtomicUsize::new(0)),
            maximum: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }

    pub(crate) fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root().join(path)
    }

    pub(crate) fn environment<I, K, V>(&self, overrides: I) -> BTreeMap<String, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut environment = self
            .environment
            .iter()
            .filter_map(|(key, value)| {
                Some((
                    key.clone().into_string().ok()?,
                    value.clone().into_string().ok()?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        environment.extend(
            overrides
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        environment
    }

    pub(crate) fn executable_on_path(&self, name: &str) -> PathBuf {
        let path = self
            .environment
            .iter()
            .find(|(key, _)| {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case("PATH"))
            })
            .map(|(_, value)| value.as_os_str())
            .expect("test sandbox captured PATH");
        let executable_names = if cfg!(windows) {
            vec![
                name.to_owned(),
                format!("{name}.exe"),
                format!("{name}.cmd"),
                format!("{name}.bat"),
            ]
        } else {
            vec![name.to_owned()]
        };
        std::env::split_paths(path)
            .flat_map(|directory| {
                executable_names
                    .iter()
                    .map(move |name| directory.join(name))
            })
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
            .unwrap_or_else(|| panic!("{name} executable was not found on captured PATH"))
    }

    pub(crate) fn run_isolated_case(
        &self,
        case: &str,
        test_name: &str,
        environment: &[(&str, &OsStr)],
    ) -> Output {
        self.run_isolated_case_without_environment(case, test_name, environment, &[])
    }

    pub(crate) fn run_isolated_case_without_environment(
        &self,
        case: &str,
        test_name: &str,
        environment: &[(&str, &OsStr)],
        removed: &[&str],
    ) -> Output {
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .envs(environment.iter().map(|(name, value)| (*name, *value)))
            .env("CODDY_STUDIO_TEST_ISOLATED_CASE", case)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in removed {
            command.env_remove(name);
        }
        let mut child = command.spawn().expect("run isolated fixture case");
        let mut stdout = child.stdout.take().expect("isolated child stdout");
        let mut stderr = child.stderr.take().expect("isolated child stderr");
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .expect("read isolated child stdout");
            bytes
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .read_to_end(&mut bytes)
                .expect("read isolated child stderr");
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait().expect("poll isolated fixture case") {
                break (status, false);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                break (
                    child.wait().expect("reap timed-out isolated fixture case"),
                    true,
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let stdout = stdout_reader.join().expect("join isolated stdout reader");
        let stderr = stderr_reader.join().expect("join isolated stderr reader");
        assert!(
            !timed_out,
            "isolated fixture case timed out:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        Output {
            status,
            stdout,
            stderr,
        }
    }

    pub(crate) fn is_isolated_case(case: &str, test_name: &str) -> bool {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        let selected = std::env::var_os("CODDY_STUDIO_TEST_ISOLATED_CASE").as_deref()
            == Some(OsStr::new(case));
        let valid_arguments = arguments
            .windows(2)
            .any(|values| values == [OsStr::new("--exact"), OsStr::new(test_name)])
            && arguments
                .iter()
                .any(|value| value == OsStr::new("--test-threads=1"));
        assert!(
            !selected || valid_arguments,
            "isolated fixture {case} requires --exact {test_name} and --test-threads=1; refusing recursive relaunch"
        );
        selected
    }

    pub(crate) fn process_input(
        &self,
        executable: impl AsRef<Path>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> ProcessRunInput {
        let mut input =
            ProcessRunInput::new(executable.as_ref().to_string_lossy().into_owned(), args);
        input.spawn_cwd = Some(self.root().to_path_buf());
        input.env = Some(self.environment(std::iter::empty::<(String, String)>()));
        input
    }

    #[cfg(unix)]
    pub(crate) fn write_executable(path: &Path, contents: &str) {
        super::executable_fixture::write_executable(path, contents);
    }

    #[cfg(unix)]
    pub(crate) fn executable_script(
        &self,
        name: &str,
        unix_body: &str,
        _windows_body: &str,
    ) -> PathBuf {
        let path = self.path(format!("{name}.sh"));
        let contents = format!("#!/bin/sh\n{unix_body}\n");
        Self::write_executable(&path, &contents);
        path
    }

    #[cfg(windows)]
    pub(crate) fn executable_script(
        &self,
        name: &str,
        _unix_body: &str,
        windows_body: &str,
    ) -> PathBuf {
        let path = self.path(format!("{name}.cmd"));
        super::executable_fixture::write_executable(&path, format!("{windows_body}\r\n"));
        path
    }

    pub(crate) fn acquire_fixture(&self) -> FixtureLease {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        FixtureLease {
            active: self.active.clone(),
        }
    }

    pub(crate) fn active_fixtures(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn maximum_active_fixtures(&self) -> usize {
        self.maximum.load(Ordering::Acquire)
    }
}

impl Drop for FixtureLease {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        io::ErrorKind,
        os::unix::process::CommandExt,
        panic::RefUnwindSafe,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Barrier,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use super::TestSandbox;

    fn assert_fixtures_run_during_fork_storm(
        case: &str,
        test_name: &str,
        label: &str,
        make_fixture: impl Fn(&TestSandbox, usize) -> PathBuf + RefUnwindSafe,
    ) {
        let sandbox = TestSandbox::new(case);
        if TestSandbox::is_isolated_case(case, test_name) {
            let stop = AtomicBool::new(false);
            let start = Barrier::new(5);
            thread::scope(|scope| {
                let workers = (0..4)
                    .map(|_| {
                        scope.spawn(|| {
                            start.wait();
                            while !stop.load(Ordering::Relaxed) {
                                let mut command = Command::new("/bin/true");
                                // SAFETY: The hook only calls async-signal-safe poll
                                // after fork; it neither allocates nor acquires locks.
                                unsafe {
                                    command.pre_exec(|| {
                                        libc::poll(std::ptr::null_mut(), 0, 2);
                                        Ok(())
                                    });
                                }
                                let status = command.status().expect("spawn fork-storm child");
                                assert!(status.success(), "fork-storm child failed: {status}");
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                start.wait();

                let result = std::panic::catch_unwind(|| {
                    // Leave room for cleanup before the isolated-case deadline under load.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    for index in 0..1000 {
                        if Instant::now() >= deadline {
                            break;
                        }
                        let fixture = make_fixture(&sandbox, index);
                        match Command::new(&fixture).status() {
                            Ok(status) => assert!(
                                status.success(),
                                "fixture {} failed: {status}",
                                fixture.display()
                            ),
                            Err(error) if error.kind() == ErrorKind::ExecutableFileBusy => {
                                panic!(
                                    "fixture {index} execution hit ETXTBSY after {index} fixtures executed: a concurrent fork inherited a writable {label} descriptor"
                                );
                            }
                            Err(error) => panic!("execute fixture {}: {error}", fixture.display()),
                        }
                    }
                });
                stop.store(true, Ordering::Relaxed);
                for worker in workers {
                    worker.join().expect("join fork-storm worker");
                }
                result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            });
            return;
        }

        let output = sandbox.run_isolated_case(case, test_name, &[]);
        assert!(
            output.status.success(),
            "isolated {label} fork storm failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn executable_scripts_run_during_concurrent_forks() {
        assert_fixtures_run_during_fork_storm(
            "executable-scripts-concurrent-forks",
            "test_support::sandbox::tests::executable_scripts_run_during_concurrent_forks",
            "script",
            |sandbox, index| sandbox.executable_script(&format!("fixture-{index}"), "exit 0", ""),
        );
    }

    #[test]
    fn copied_executables_run_during_concurrent_forks() {
        assert_fixtures_run_during_fork_storm(
            "copied-executables-concurrent-forks",
            "test_support::sandbox::tests::copied_executables_run_during_concurrent_forks",
            "copied executable",
            |sandbox, index| {
                let fixture = sandbox.path(format!("fixture-{index}"));
                crate::test_support::executable_fixture::copy_executable(
                    Path::new("/bin/true"),
                    &fixture,
                );
                fixture
            },
        );
    }
}
