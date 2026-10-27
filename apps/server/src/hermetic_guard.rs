//! Guard provider and hosting boundaries only in Cargo dev-unit builds.

use std::{
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
};

use crate::process::{Platform, launch_executable_extensions, locate_executable};

const DEFAULT_MODE: &str = "abort";
const MODE_ENV: &str = "CODDY_STUDIO_HERMETIC_GUARD";
const GUARDED_PROGRAMS: &[&str] = &[
    "claude",
    "codex",
    "coddy",
    "opencode",
    "cursor-agent",
    "agent",
    "grok",
    "gh",
    "glab",
    "az",
    "jj",
    "npm",
    "npx",
    "bun",
    "pnpm",
    "yarn",
    "brew",
    "vp",
    "winget",
];

fn abort_guard_process() -> ! {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe {
            let _ = libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Pipe core_pattern handlers such as systemd-coredump still dump when
        // RLIMIT_CORE is 0 unless the process is not dumpable.
        unsafe {
            let _ = libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
    }
    std::process::abort()
}

fn abort_mode() -> bool {
    match std::env::var_os(MODE_ENV) {
        None => DEFAULT_MODE == "abort",
        Some(value) if value == "report" => false,
        Some(value) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "hermetic-test-guard: invalid {MODE_ENV} value {value:?} (thread {})",
                std::thread::current().name().unwrap_or("unnamed")
            );
            abort_guard_process();
        }
    }
}

pub(crate) fn refuse_access(name: &str, path: &Path) -> bool {
    let abort = abort_mode();
    let _ = writeln!(
        std::io::stderr().lock(),
        "hermetic-test-guard: refused {name} at {} (thread {})",
        path.display(),
        std::thread::current().name().unwrap_or("unnamed")
    );
    if abort {
        abort_guard_process();
    }
    false
}

pub(crate) fn is_guarded_program(program: &Path) -> bool {
    program
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| {
            GUARDED_PROGRAMS.iter().any(|name| {
                if cfg!(windows) {
                    stem.eq_ignore_ascii_case(name)
                } else {
                    stem == *name
                }
            })
        })
}

fn cargo_profile_root(executable: &Path) -> Option<&Path> {
    let parent = executable.parent()?;
    let profile = if parent
        .file_name()
        .is_some_and(|name| name == "deps" || name == "examples")
    {
        parent.parent()?
    } else {
        parent
    };
    profile.join(".fingerprint").is_dir().then_some(profile)
}

fn allowed_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        std::env::temp_dir(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
    ];
    if let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") {
        roots.push(PathBuf::from(manifest).join("tests/fixtures"));
    }
    if let Ok(executable) = std::env::current_exe().and_then(std::fs::canonicalize)
        && let Some(profile) = cargo_profile_root(&executable)
    {
        roots.push(profile.to_path_buf());
    }
    roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .filter(|root| root.is_dir())
        .collect()
}

fn is_allowed(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn isolated_search_path(search_path: Option<&OsStr>, roots: &[PathBuf]) -> bool {
    let Some(search_path) = search_path else {
        return false;
    };
    std::env::split_paths(search_path).all(|entry| {
        if entry.as_os_str().is_empty() || !entry.is_absolute() {
            return false;
        }
        match std::fs::canonicalize(entry) {
            Ok(entry) => is_allowed(&entry, roots),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    })
}

pub(crate) fn resolve_guarded_executable(
    program: &Path,
    cwd: Option<&Path>,
    search_path: Option<&OsStr>,
    extensions: &[String],
    prefer_current_file: bool,
) -> Option<PathBuf> {
    let _ = abort_mode();
    let roots = allowed_roots();
    let bare = !program.is_absolute() && program.components().count() == 1;
    if bare
        && search_path == std::env::var_os("PATH").as_deref()
        && !isolated_search_path(search_path, &roots)
    {
        refuse_access(&program.display().to_string(), Path::new("<ambient PATH>"));
        return None;
    }
    // The provider runtime historically checks an existing CWD file before
    // its explicit search path. Preserve that only after inherited-PATH
    // preflight; terminal and hosting resolution use ordinary PATH ordering.
    let candidate = if prefer_current_file && program.is_file() {
        program.to_path_buf()
    } else if bare {
        locate_executable(program.to_str()?, cwd, search_path, extensions)?
    } else {
        let candidate = if program.is_absolute() {
            program.to_path_buf()
        } else {
            cwd?.join(program)
        };
        if candidate.is_file() {
            candidate
        } else if cfg!(windows) && candidate.extension().is_none() {
            extensions.iter().find_map(|extension| {
                let mut name = candidate.as_os_str().to_owned();
                name.push(extension);
                let candidate = PathBuf::from(name);
                candidate.is_file().then_some(candidate)
            })?
        } else {
            return None;
        }
    };
    let canonical = match std::fs::canonicalize(&candidate) {
        Ok(canonical) => canonical,
        Err(_) => {
            refuse_access(&program.display().to_string(), &candidate);
            return None;
        }
    };
    if !is_allowed(&canonical, &roots) {
        refuse_access(&program.display().to_string(), &canonical);
        return None;
    }
    Some(if candidate.is_absolute() {
        candidate
    } else {
        canonical
    })
}

pub(crate) fn checked_launch_executable(
    program: &Path,
    cwd: Option<&Path>,
    search_path: Option<&OsStr>,
) -> Option<PathBuf> {
    if !is_guarded_program(program) {
        return Some(program.to_path_buf());
    }
    checked_provider_executable(program, cwd, search_path)
}

/// Provider-only boundaries know the executable's role even when an installed
/// version or fixture has a basename such as `2.1.220` instead of `claude`.
pub(crate) fn checked_provider_executable(
    program: &Path,
    cwd: Option<&Path>,
    search_path: Option<&OsStr>,
) -> Option<PathBuf> {
    let extensions = launch_executable_extensions(Platform::current(), None);
    resolve_guarded_executable(program, cwd, search_path, &extensions, false)
}

pub(crate) fn credential_path_allowed(name: &str, path: &Path) -> bool {
    let _ = abort_mode();
    match std::fs::canonicalize(path) {
        Ok(canonical) if is_allowed(&canonical, &allowed_roots()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        _ => refuse_access(name, path),
    }
}

#[cfg(all(test, unix))]
pub(crate) fn refusal_fixture(test: &str, program: &str) -> Option<PathBuf> {
    use crate::test_support::{TestSandbox, executable_fixture};
    let case = "guard-direct-boundary";
    if TestSandbox::is_isolated_case(case, test) {
        return Some(PathBuf::from(
            std::env::var_os("CODDY_STUDIO_GUARD_EXECUTABLE").unwrap(),
        ));
    }
    let sandbox = TestSandbox::new(program);
    let allowed = sandbox.path("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let executable = sandbox.path(program);
    let trace = sandbox.path("spawned");
    executable_fixture::write_executable(
        &executable,
        b"#!/bin/sh\nprintf spawned > \"$CODDY_STUDIO_GUARD_TRACE\"\n",
    );
    let output = sandbox.run_isolated_case(
        case,
        test,
        &[
            ("TMPDIR", allowed.as_os_str()),
            ("TMP", allowed.as_os_str()),
            ("TEMP", allowed.as_os_str()),
            ("HOME", allowed.as_os_str()),
            ("USERPROFILE", allowed.as_os_str()),
            ("CODDY_STUDIO_GUARD_EXECUTABLE", executable.as_os_str()),
            ("CODDY_STUDIO_GUARD_TRACE", trace.as_os_str()),
            (MODE_ENV, OsStr::new("report")),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !trace.exists(),
        "guard must reject before executing the owned tripwire"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused"));
    None
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        path::{Path, PathBuf},
    };

    use crate::test_support::{TestSandbox, executable_fixture};

    #[test]
    fn malformed_isolated_child_arguments_fail_before_any_relaunch() {
        const CASE: &str = "guard-malformed-reexec";
        const TEST: &str =
            "hermetic_guard::tests::malformed_isolated_child_arguments_fail_before_any_relaunch";
        if std::env::var_os("CODDY_STUDIO_TEST_ISOLATED_CASE").as_deref() == Some(OsStr::new(CASE))
        {
            assert!(TestSandbox::is_isolated_case(CASE, TEST));
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("CODDY_STUDIO_TEST_ISOLATED_CASE", CASE)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("refusing recursive relaunch"));
    }

    fn assert_default_abort(output: std::process::Output, program: &str) {
        assert!(!output.status.success(), "unguarded child must fail");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(libc::SIGABRT));
        }
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains(&format!("hermetic-test-guard: refused {program}")),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("thread"), "{diagnostic}");
    }

    fn run_default_mode_child(
        sandbox: &TestSandbox,
        case: &str,
        test: &str,
    ) -> std::process::Output {
        let path =
            std::env::join_paths([sandbox.root(), Path::new(env!("CARGO_MANIFEST_DIR"))]).unwrap();
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .current_dir(sandbox.root())
            .env("CODDY_STUDIO_TEST_ISOLATED_CASE", case)
            .env("PATH", path)
            .env("HOME", sandbox.root())
            .env("USERPROFILE", sandbox.root())
            .env("XDG_CONFIG_HOME", sandbox.root())
            .env_remove("CODDY_STUDIO_HERMETIC_GUARD")
            .output()
            .unwrap()
    }

    #[test]
    fn default_mode_aborts_before_ambient_resolution_even_in_spawned_tasks() {
        const CASE: &str = "guard-default-abort";
        const TEST: &str = "hermetic_guard::tests::default_mode_aborts_before_ambient_resolution_even_in_spawned_tasks";
        let sandbox = TestSandbox::new(CASE);
        if TestSandbox::is_isolated_case(CASE, TEST) {
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async {
                    tokio::spawn(async {
                        let _ = crate::production::provider_runtime::resolve_provider_executable_in_path("codex", std::env::var_os("PATH").as_deref());
                    }).await.unwrap();
                });
            }).join().unwrap();
            panic!("default mode did not abort inherited helper/task resolution");
        }
        assert_default_abort(run_default_mode_child(&sandbox, CASE, TEST), "codex");
    }

    #[tokio::test]
    async fn bare_runtime_without_hermetic_settings_aborts_before_provider_discovery() {
        const CASE: &str = "guard-bare-runtime";
        const TEST: &str = "hermetic_guard::tests::bare_runtime_without_hermetic_settings_aborts_before_provider_discovery";
        let sandbox = TestSandbox::new(CASE);
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let config = crate::ServerConfig::new(sandbox.root())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth();
            // Keep unrelated network work disabled, without pinning provider paths
            // or using the hermetic settings helper whose missing call is under test.
            std::fs::create_dir_all(config.state_dir()).unwrap();
            std::fs::write(
                config.state_dir().join("settings.json"),
                br#"{"enableProviderUpdateChecks":false}"#,
            )
            .unwrap();
            let handle = crate::ServerRuntime::start(config).await.unwrap();
            handle.shutdown();
            handle.join().await.unwrap();
            panic!("unseeded runtime did not abort provider discovery");
        }
        // Every possible built-in is a disposable tripwire. Even a broken guard
        // could not execute a developer-installed provider through this PATH.
        for name in ["codex", "claude", "cursor-agent", "grok", "opencode"] {
            let name = if cfg!(windows) {
                format!("{name}.cmd")
            } else {
                name.to_owned()
            };
            let script: &[u8] = if cfg!(windows) {
                b"@exit /b 0\r\n"
            } else {
                b"#!/bin/sh\nexit 0\n"
            };
            executable_fixture::write_executable(&sandbox.path(name), script);
        }
        assert_default_abort(run_default_mode_child(&sandbox, CASE, TEST), "codex");
    }

    #[cfg(unix)]
    fn hosting_request(command: PathBuf, sandbox: &TestSandbox) -> crate::git::ProcessRequest {
        crate::git::ProcessRequest {
            operation: "guard-fixture".to_owned(),
            command,
            args: vec![],
            cwd: sandbox.root().to_path_buf(),
            env: vec![],
            stdin: None,
            timeout: std::time::Duration::from_secs(2),
            max_output_bytes: 1024,
            output_policy: crate::git::OutputPolicy::Truncate,
            append_truncation_marker: false,
            allow_non_zero_exit: false,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ambient_hosting_requests_are_refused_before_text_and_bytes_spawns() {
        const CASE: &str = "guard-hosting-report";
        const TEST: &str = "hermetic_guard::tests::ambient_hosting_requests_are_refused_before_text_and_bytes_spawns";
        let sandbox = TestSandbox::new("guard-hosting-report");
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let request = hosting_request("gh".into(), &sandbox);
            let text = crate::git::ProcessRunner
                .run(request.clone(), &cancellation)
                .await;
            let bytes = crate::git::ProcessRunner
                .run_bytes(request, &cancellation)
                .await;
            for result in [text.map(|_| ()), bytes.map(|_| ())] {
                assert!(
                    matches!(result, Err(crate::git::ProcessError::Spawn { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
                );
            }
            return;
        }
        let trace = sandbox.path("spawned");
        executable_fixture::write_executable(
            &sandbox.path("gh"),
            b"#!/bin/sh\nprintf 'spawned' > \"$CODDY_STUDIO_GUARD_TRACE\"\n",
        );
        let search_path =
            std::env::join_paths([sandbox.root(), Path::new(env!("CARGO_MANIFEST_DIR"))]).unwrap();
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("PATH", search_path.as_os_str()),
                ("CODDY_STUDIO_GUARD_TRACE", trace.as_os_str()),
                ("CODDY_STUDIO_HERMETIC_GUARD", OsStr::new("report")),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !trace.exists(),
            "both hosting run paths must reject before spawning"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused gh")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_fixture_path_spawns_the_checked_candidate_in_both_hosting_paths() {
        let sandbox = TestSandbox::new("guard-hosting-allowed");
        executable_fixture::write_executable(
            &sandbox.path("gh"),
            b"#!/bin/sh\nprintf 'fixture-host'\n",
        );
        let mut request = hosting_request("gh".into(), &sandbox);
        request
            .env
            .push(("PATH".into(), sandbox.root().as_os_str().to_owned()));
        let cancellation = tokio_util::sync::CancellationToken::new();
        let text = crate::git::ProcessRunner
            .run(request.clone(), &cancellation)
            .await
            .unwrap();
        let bytes = crate::git::ProcessRunner
            .run_bytes(request, &cancellation)
            .await
            .unwrap();
        assert_eq!(text.stdout, "fixture-host");
        assert_eq!(bytes.stdout, b"fixture-host");
    }

    #[test]
    fn explicit_outside_root_executable_is_refused_and_missing_paths_stay_missing() {
        const CASE: &str = "guard-outside-root";
        const TEST: &str = "hermetic_guard::tests::explicit_outside_root_executable_is_refused_and_missing_paths_stay_missing";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let executable = std::env::var("CODDY_STUDIO_GUARD_EXECUTABLE").unwrap();
            assert!(
                crate::production::provider_runtime::resolve_provider_executable_in_path(
                    &executable,
                    None
                )
                .is_none()
            );
            let missing = std::env::temp_dir().join("missing/codex");
            assert!(
                crate::production::provider_runtime::resolve_provider_executable_in_path(
                    &missing.display().to_string(),
                    None
                )
                .is_none()
            );
            #[cfg(unix)]
            {
                let link = std::env::temp_dir().join("codex");
                std::os::unix::fs::symlink(&executable, &link).unwrap();
                assert!(
                    crate::production::provider_runtime::resolve_provider_executable_in_path(
                        &link.display().to_string(),
                        None
                    )
                    .is_none(),
                    "a fixture-root symlink must not expose an outside-root executable"
                );
            }
            return;
        }
        let sandbox = TestSandbox::new("guard-outside-root");
        let allowed = sandbox.path("allowed");
        std::fs::create_dir(&allowed).unwrap();
        let executable = sandbox.path(if cfg!(windows) { "codex.exe" } else { "codex" });
        executable_fixture::write_executable(&executable, b"fixture; this path is never spawned");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("TMPDIR", allowed.as_os_str()),
                ("TMP", allowed.as_os_str()),
                ("TEMP", allowed.as_os_str()),
                ("CODDY_STUDIO_GUARD_EXECUTABLE", executable.as_os_str()),
                ("CODDY_STUDIO_HERMETIC_GUARD", OsStr::new("report")),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused"));
    }

    #[test]
    fn isolated_process_path_and_explicit_fixture_executables_are_allowed() {
        const CASE: &str = "guard-isolated-path";
        const TEST: &str = "hermetic_guard::tests::isolated_process_path_and_explicit_fixture_executables_are_allowed";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let executable =
                PathBuf::from(std::env::var_os("CODDY_STUDIO_GUARD_EXECUTABLE").unwrap());
            for input in ["codex", executable.to_str().unwrap()] {
                assert_eq!(
                    crate::production::provider_runtime::resolve_provider_executable_in_path(
                        input,
                        std::env::var_os("PATH").as_deref()
                    ),
                    Some(executable.clone())
                );
            }
            return;
        }
        let sandbox = TestSandbox::new("guard-isolated-path");
        let executable = sandbox.path(if cfg!(windows) { "codex.cmd" } else { "codex" });
        executable_fixture::write_executable(&executable, b"fixture; resolution only");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("PATH", sandbox.root().as_os_str()),
                ("CODDY_STUDIO_GUARD_EXECUTABLE", executable.as_os_str()),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cargo_profile_root_requires_fingerprint_for_every_supported_binary_location() {
        let sandbox = TestSandbox::new("guard-profile-proof");
        let profile = sandbox.path("debug");
        std::fs::create_dir_all(profile.join("deps")).unwrap();
        std::fs::create_dir_all(profile.join("examples")).unwrap();
        let locations = [
            profile.join("coddy-studio"),
            profile.join("deps/test"),
            profile.join("examples/example"),
        ];
        for executable in &locations {
            assert!(super::cargo_profile_root(executable).is_none());
        }
        std::fs::create_dir(profile.join(".fingerprint")).unwrap();
        for executable in &locations {
            assert_eq!(
                super::cargo_profile_root(executable),
                Some(profile.as_path())
            );
        }
    }

    #[test]
    fn runtime_manifest_fixture_root_is_allowed_outside_the_child_temporary_root() {
        const CASE: &str = "guard-runtime-manifest";
        const TEST: &str = "hermetic_guard::tests::runtime_manifest_fixture_root_is_allowed_outside_the_child_temporary_root";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let executable =
                PathBuf::from(std::env::var_os("CODDY_STUDIO_GUARD_EXECUTABLE").unwrap());
            assert_eq!(
                crate::production::provider_runtime::resolve_provider_executable_in_path(
                    executable.to_str().unwrap(),
                    None
                ),
                Some(executable)
            );
            return;
        }
        let sandbox = TestSandbox::new(CASE);
        let temporary = sandbox.path("tmp");
        std::fs::create_dir(&temporary).unwrap();
        let fixture_root = sandbox.path("tests/fixtures");
        std::fs::create_dir_all(&fixture_root).unwrap();
        let executable = fixture_root.join(if cfg!(windows) { "codex.cmd" } else { "codex" });
        executable_fixture::write_executable(&executable, b"fixture; resolution only");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("TMPDIR", temporary.as_os_str()),
                ("TMP", temporary.as_os_str()),
                ("TEMP", temporary.as_os_str()),
                ("CARGO_MANIFEST_DIR", sandbox.root().as_os_str()),
                ("CODDY_STUDIO_GUARD_EXECUTABLE", executable.as_os_str()),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_explicit_extensionless_provider_uses_checked_launch_extensions() {
        let sandbox = TestSandbox::new("guard-windows-extension");
        let executable = sandbox.path("CoDeX.cmd");
        executable_fixture::write_executable(&executable, b"@exit /b 0\r\n");
        assert!(super::is_guarded_program(&executable));
        assert_eq!(
            super::checked_launch_executable(&sandbox.path("CoDeX"), None, None),
            Some(executable)
        );
    }

    #[test]
    fn inherited_path_ignores_missing_entries_and_refuses_empty_relative_or_unreadable_entries() {
        const CASE: &str = "guard-path-entries";
        const TEST: &str = "hermetic_guard::tests::inherited_path_ignores_missing_entries_and_refuses_empty_relative_or_unreadable_entries";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let result = crate::production::provider_runtime::resolve_provider_executable_in_path(
                "codex",
                std::env::var_os("PATH").as_deref(),
            );
            assert_eq!(
                result.is_some(),
                std::env::var("CODDY_STUDIO_GUARD_EXPECT_ALLOWED").unwrap() == "yes"
            );
            return;
        }
        let sandbox = TestSandbox::new("guard-path-entries");
        let executable = sandbox.path(if cfg!(windows) { "codex.exe" } else { "codex" });
        executable_fixture::write_executable(&executable, b"fixture; resolution only");
        let missing = sandbox.path("missing");
        let paths = vec![
            (
                std::env::join_paths([sandbox.root(), missing.as_path()]).unwrap(),
                true,
            ),
            (
                std::env::join_paths([sandbox.root(), Path::new("")]).unwrap(),
                false,
            ),
            (
                std::env::join_paths([sandbox.root(), Path::new("relative")]).unwrap(),
                false,
            ),
        ];
        #[cfg(unix)]
        let paths = {
            let mut paths = paths;
            let loop_path = sandbox.path("loop");
            std::os::unix::fs::symlink("loop", &loop_path).unwrap();
            paths.push((
                std::env::join_paths([sandbox.root(), loop_path.as_path()]).unwrap(),
                false,
            ));
            paths
        };
        for (path, allowed) in paths {
            let output = sandbox.run_isolated_case(
                CASE,
                TEST,
                &[
                    ("PATH", path.as_os_str()),
                    (
                        "CODDY_STUDIO_GUARD_EXPECT_ALLOWED",
                        OsStr::new(if allowed { "yes" } else { "no" }),
                    ),
                    ("CODDY_STUDIO_HERMETIC_GUARD", OsStr::new("report")),
                ],
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused"),
                !allowed
            );
        }
    }

    #[cfg(unix)]
    fn raise_parent_core_limit() {
        let limit = libc::rlimit {
            rlim_cur: libc::RLIM_INFINITY,
            rlim_max: libc::RLIM_INFINITY,
        };
        let status = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) };
        assert_eq!(
            status,
            0,
            "raise RLIMIT_CORE so a dumpable abort would write a core: {}",
            std::io::Error::last_os_error()
        );
    }

    /// Directory for file `core_pattern` values. Pipe handlers return `None`.
    #[cfg(target_os = "linux")]
    fn linux_file_core_directory(cwd: &Path) -> Option<PathBuf> {
        let pattern = std::fs::read_to_string("/proc/sys/kernel/core_pattern").ok()?;
        let pattern = pattern.trim();
        if pattern.is_empty() || pattern.starts_with('|') {
            return None;
        }
        Some(match pattern.rfind('/') {
            Some(0) => PathBuf::from("/"),
            Some(index) => PathBuf::from(&pattern[..index]),
            None => cwd.to_path_buf(),
        })
    }

    // libtest names the aborting thread from the test path; Linux keeps 15 bytes
    // in `comm`, and `%e` in core_pattern uses that (`hermetic_guard:` here).
    #[cfg(target_os = "linux")]
    fn linux_abort_comm_cores(directory: &Path, test_name: &str) -> Vec<PathBuf> {
        let comm = &test_name[..test_name.len().min(15)];
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.contains(comm) {
                paths.push(entry.path());
            }
        }
        paths.sort();
        paths
    }

    #[test]
    fn invalid_mode_aborts_with_raw_stderr_instead_of_allowing_an_off_switch() {
        const CASE: &str = "guard-invalid-mode";
        const TEST: &str = "hermetic_guard::tests::invalid_mode_aborts_with_raw_stderr_instead_of_allowing_an_off_switch";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let child_sandbox = TestSandbox::new("guard-invalid-mode-child");
            let cwd = std::env::var_os("CODDY_STUDIO_GUARD_ABORT_CWD")
                .map(PathBuf::from)
                .unwrap_or_else(|| child_sandbox.root().to_path_buf());
            std::env::set_current_dir(cwd).unwrap();
            let _ = crate::production::provider_runtime::resolve_provider_executable_in_path(
                "codex",
                std::env::var_os("PATH").as_deref(),
            );
            panic!("invalid guard mode must abort");
        }
        let sandbox = TestSandbox::new("guard-invalid-mode");
        #[cfg(unix)]
        raise_parent_core_limit();
        #[cfg(target_os = "linux")]
        let core_directory = linux_file_core_directory(sandbox.root());
        #[cfg(target_os = "linux")]
        let cores_before = core_directory
            .as_ref()
            .map(|directory| linux_abort_comm_cores(directory, TEST));
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("CODDY_STUDIO_HERMETIC_GUARD", OsStr::new("off")),
                ("CODDY_STUDIO_GUARD_ABORT_CWD", sandbox.root().as_os_str()),
            ],
        );
        assert!(!output.status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(libc::SIGABRT));
        }
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("invalid CODDY_STUDIO_HERMETIC_GUARD value \"off\"")
        );
        #[cfg(target_os = "linux")]
        if let (Some(directory), Some(before)) = (core_directory.as_ref(), cores_before.as_ref()) {
            let after = linux_abort_comm_cores(directory, TEST);
            let created: Vec<_> = after.iter().filter(|path| !before.contains(path)).collect();
            assert!(
                created.is_empty(),
                "abort wrote a core dump in {}: {created:?}",
                directory.display()
            );
        }
    }

    #[test]
    fn ambient_provider_lookup_is_refused_before_searching_a_test_fake() {
        const CASE: &str = "guard-provider-report";
        const TEST: &str = "hermetic_guard::tests::ambient_provider_lookup_is_refused_before_searching_a_test_fake";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            assert!(
                crate::production::provider_runtime::resolve_provider_executable_in_path(
                    "codex",
                    std::env::var_os("PATH").as_deref(),
                )
                .is_none(),
                "a mixed ambient PATH must be refused even when its first executable is a fake"
            );
            return;
        }
        let sandbox = TestSandbox::new("guard-provider-report");
        let executable = sandbox.path(if cfg!(windows) { "codex.cmd" } else { "codex" });
        executable_fixture::write_executable(&executable, b"#!/bin/sh\nexit 0\n");
        let search_path =
            std::env::join_paths([sandbox.root(), Path::new(env!("CARGO_MANIFEST_DIR"))])
                .expect("fixture and out-of-root PATH");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("PATH", search_path.as_os_str()),
                ("CODDY_STUDIO_HERMETIC_GUARD", OsStr::new("report")),
            ],
        );
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("hermetic-test-guard: refused codex")
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_provider_with_a_versioned_basename_cannot_bypass_root_checks() {
        let Some(executable) = super::refusal_fixture(
            "hermetic_guard::tests::explicit_provider_with_a_versioned_basename_cannot_bypass_root_checks",
            "2.1.220",
        ) else {
            return;
        };
        assert!(
            crate::production::provider_runtime::resolve_provider_executable_in_path(
                executable.to_str().unwrap(),
                None
            )
            .is_none()
        );
        assert!(
            crate::production::provider_runtime::prepare_provider_launch(
                &executable,
                std::iter::empty::<&str>()
            )
            .is_err()
        );
    }
}
