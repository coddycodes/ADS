#![cfg(target_os = "linux")]

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{collections::BTreeMap, time::Duration};

use coddy_studio_server::{
    git::{OutputPolicy, ProcessRequest, ProcessRunner},
    terminal::{PortablePtyBackend, PtyBackend, PtyExit, PtySpawnInput},
};
use tokio_util::sync::CancellationToken;

#[path = "support/reexec.rs"]
mod reexec;

#[path = "support/appimage_environment.rs"]
mod appimage_environment;
use appimage_environment::{
    ENVIRONMENT_CASES, HOST_PATH_FIXTURE, assert_child_environment, check_inherited_environment,
};

async fn run_pty_probe(input: PtySpawnInput) -> PtyExit {
    let process = PortablePtyBackend.spawn(&input).expect("real PTY child");
    let mut exit = process.subscribe_exit();
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        exit.wait_for(|value| value.is_some())
            .await
            .unwrap()
            .clone()
            .unwrap()
    })
    .await;
    if completed.is_err() {
        process.kill().expect("stop timed-out PTY probe");
    }
    completed.expect("PTY probe completed")
}

#[test]
fn terminal_children_ignore_appimage_environment() {
    check_inherited_environment(
        "terminal_children_ignore_appimage_environment",
        ENVIRONMENT_CASES,
        |expected| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let capture = directory.path().join("environment");
                let completed = run_pty_probe(PtySpawnInput {
                    executable: "/bin/sh".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        "/usr/bin/env -0 > \"$1\"".to_owned(),
                        "probe".to_owned(),
                        capture.to_str().unwrap().to_owned(),
                    ],
                    cwd: directory.path().to_path_buf(),
                    cols: 80,
                    rows: 24,
                    env: BTreeMap::new(),
                })
                .await;
                assert_eq!(completed.exit_code, Some(0));
                assert_child_environment(&std::fs::read(capture).unwrap(), expected);
            });
        },
    );
}

#[test]
fn git_children_ignore_appimage_environment() {
    check_inherited_environment(
        "git_children_ignore_appimage_environment",
        ENVIRONMENT_CASES,
        |expected| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let request = ProcessRequest {
                    operation: "AppImage environment probe".to_owned(),
                    command: "/usr/bin/env".into(),
                    args: vec!["-0".into()],
                    cwd: std::env::temp_dir(),
                    env: Vec::new(),
                    stdin: None,
                    timeout: Duration::from_secs(10),
                    max_output_bytes: 128 * 1024,
                    output_policy: OutputPolicy::Error,
                    append_truncation_marker: false,
                    allow_non_zero_exit: false,
                };
                let output = ProcessRunner
                    .run_bytes(request, &CancellationToken::new())
                    .await
                    .expect("Git runner environment probe");
                assert_child_environment(&output.stdout, expected);
            });
        },
    );
}

#[test]
fn extracted_appdir_isolates_real_child_commands() {
    use coddy_studio_server::process::isolate_appimage_environment;
    use std::{ffi::OsString, path::PathBuf};

    const TEST: &str = "extracted_appdir_isolates_real_child_commands";
    const PHASE: &str = "copied-executable";
    if let Some(child) = reexec::enter(TEST, PHASE) {
        let appdir = PathBuf::from(std::env::var_os("APPDIR").unwrap());
        assert!(std::env::current_exe().unwrap().starts_with(&appdir));
        assert!(std::env::var_os("APPIMAGE").is_none_or(|value| value.is_empty()));
        let parent = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let isolate = std::env::var("CODDY_STUDIO_TEST_EXPECT_ISOLATION").unwrap() == "yes";
        let expected = if isolate {
            BTreeMap::from([
                ("APPDIR", None),
                ("LD_LIBRARY_PATH", Some(OsString::from("/host/keep"))),
                ("PYTHONHOME", None),
                (
                    "CODDY_STUDIO_TEST_UNRELATED",
                    Some(OsString::from("keep:exactly")),
                ),
            ])
        } else {
            [
                "APPDIR",
                "LD_LIBRARY_PATH",
                "PYTHONHOME",
                "CODDY_STUDIO_TEST_UNRELATED",
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect()
        };
        let mut command = std::process::Command::new("/usr/bin/env");
        command.arg("-0");
        isolate_appimage_environment(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_child_environment(&output.stdout, &expected);

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut command = tokio::process::Command::new("/usr/bin/env");
            command.arg("-0").kill_on_drop(true);
            isolate_appimage_environment(&mut command);
            let output = tokio::time::timeout(Duration::from_secs(10), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(output.status.success());
            assert_child_environment(&output.stdout, &expected);

            let directory = tempfile::tempdir().unwrap();
            let capture = directory.path().join("pty-environment");
            let completed = run_pty_probe(PtySpawnInput {
                executable: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "/usr/bin/env -0 > \"$1\"".to_owned(),
                    "probe".to_owned(),
                    capture.to_str().unwrap().to_owned(),
                ],
                cwd: directory.path().to_owned(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .await;
            assert_eq!(completed.exit_code, Some(0));
            assert_child_environment(&std::fs::read(capture).unwrap(), &expected);
        });
        assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), parent);
        child.complete();
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let appdir = directory.path().join("AppDir");
    let bin = appdir.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("appimage-test");
    executable_fixture::copy_executable(&std::env::current_exe().unwrap(), &executable);
    let apprun = appdir.join("AppRun");
    std::fs::write(&apprun, "fixture launcher").unwrap();
    for case in ["unset", "empty", "missing-launcher"] {
        if case == "missing-launcher" {
            std::fs::remove_file(&apprun).unwrap();
        }
        reexec::run(TEST, PHASE, Some(&executable), |command| {
            command
                .env("APPDIR", &appdir)
                .env(
                    "LD_LIBRARY_PATH",
                    format!("{}/usr/lib:/host/keep", appdir.display()),
                )
                .env("PYTHONHOME", appdir.join("usr"))
                .env("CODDY_STUDIO_TEST_UNRELATED", "keep:exactly")
                .env(
                    "CODDY_STUDIO_TEST_EXPECT_ISOLATION",
                    if case == "missing-launcher" {
                        "no"
                    } else {
                        "yes"
                    },
                );
            if case == "empty" {
                command.env("APPIMAGE", "");
            } else {
                command.env_remove("APPIMAGE");
            }
        });
    }
}

#[test]
fn python_in_appimage_terminal_uses_host_standard_library() {
    let available = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .env(
            "PATH",
            std::env::var_os(HOST_PATH_FIXTURE)
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default(),
        )
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .output();
    let python = match available {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "CODDY_STUDIO_PYTHON_PTY_SKIP: python3 is not on the host PATH; unavailable evidence"
            );
            return;
        }
        output => {
            let output = output.expect("probe host python3");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }
    };
    check_inherited_environment(
        "python_in_appimage_terminal_uses_host_standard_library",
        &["mixed"],
        |_| {
            let pythonhome = std::env::var_os("PYTHONHOME").expect("bundled Python home");
            assert_eq!(std::fs::read_dir(pythonhome).unwrap().count(), 0);
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let capture = directory.path().join("python-output");
                let completed = run_pty_probe(PtySpawnInput {
                    executable: "/bin/sh".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        "exec \"$1\" -c 'print(\"ok\")' > \"$2\" 2>&1".to_owned(),
                        "probe".to_owned(),
                        python,
                        capture.to_str().unwrap().to_owned(),
                    ],
                    cwd: directory.path().to_path_buf(),
                    cols: 80,
                    rows: 24,
                    env: BTreeMap::new(),
                })
                .await;
                let output = std::fs::read_to_string(capture).unwrap();
                assert_eq!(completed.exit_code, Some(0), "{output}");
                assert_eq!(output, "ok\n");
            });
        },
    );
    println!("CODDY_STUDIO_PYTHON_PTY_RAN: python3 exited 0 and printed ok");
}
