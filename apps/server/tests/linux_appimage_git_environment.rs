#![cfg(target_os = "linux")]

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use coddy_studio_server::git::{OutputPolicy, ProcessRequest, ProcessRunner};
use tokio_util::sync::CancellationToken;

#[path = "support/reexec.rs"]
mod reexec;

const TEST: &str = "git_subprocesses_ignore_appimage_libraries";
const PHASE: &str = "git-loader";
const HELPER_ARGS: [&str; 2] = ["origin", "https://127.0.0.1:1/coddy-studio-test.git"];

fn request(command: impl Into<PathBuf>, args: &[&str]) -> ProcessRequest {
    ProcessRequest {
        operation: "appimage-git-regression".to_owned(),
        command: command.into(),
        args: args.iter().map(OsString::from).collect(),
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        stdin: None,
        timeout: Duration::from_secs(15),
        max_output_bytes: 8192,
        output_policy: OutputPolicy::Error,
        append_truncation_marker: false,
        allow_non_zero_exit: true,
    }
}

// Re-exec isolates inherited AppImage state from the parallel test harness.
// A deliberately incompatible libcurl reproduces the same loader collision as
// the packaged libnghttp2, without depending on one distro's curl ABI/version.
#[test]
fn git_subprocesses_ignore_appimage_libraries() {
    if let Some(child) = reexec::enter(TEST, PHASE) {
        tokio::runtime::Runtime::new()
            .expect("Tokio runtime")
            .block_on(assert_git_children());
        child.complete();
        return;
    }

    let fixture = tempfile::tempdir().expect("fixture directory");
    let appdir = fixture.path().join("Extracted AppImage");
    let bundled = appdir.join("usr/lib");
    let stale = fixture.path().join(".mount_bibcodOLD/usr/lib");
    let custom = fixture.path().join("Extracted AppImage-custom/lib");
    for path in [&bundled, &stale, &custom] {
        fs::create_dir_all(path).expect("library directory");
    }
    let exec_path = Command::new("git")
        .arg("--exec-path")
        .env_remove("LD_LIBRARY_PATH")
        .output()
        .expect("system Git");
    assert!(exec_path.status.success());
    let helper =
        Path::new(String::from_utf8_lossy(&exec_path.stdout).trim()).join("git-remote-https");
    let linked = Command::new("ldd")
        .arg(&helper)
        .env_remove("LD_LIBRARY_PATH")
        .output()
        .expect("inspect system Git's library dependencies");
    assert!(linked.status.success());
    let dependencies = String::from_utf8_lossy(&linked.stdout);
    // Debian/Ubuntu commonly use libcurl-gnutls; Fedora/Arch use libcurl.
    // Select the actual dependency rather than encode a distro-specific name.
    let curl_library = dependencies
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|name| name.starts_with("libcurl") && name.contains(".so."))
        .expect("system HTTPS helper links libcurl");
    let source = fixture.path().join("incompatible.c");
    fs::write(&source, "void coddy_studio_incompatible_library(void) {}\n")
        .expect("incompatible library source");
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(format!("-Wl,-soname,{curl_library}"))
        .arg("-o")
        .arg(bundled.join(curl_library))
        .arg(&source)
        .env_remove("LD_LIBRARY_PATH")
        .output()
        .expect("C compiler is required for the Linux loader regression");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::copy(bundled.join(curl_library), stale.join(curl_library)).expect("stale AppImage library");
    let library_path = std::env::join_paths([&bundled, &stale, &custom]).expect("library path");

    let broken = Command::new(&helper)
        .args(HELPER_ARGS)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LD_LIBRARY_PATH", &library_path)
        .output()
        .expect("HTTPS helper with incompatible library");
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("symbol lookup error"),
        "fixture must reproduce the loader failure: {:?}\n{}",
        broken.status,
        String::from_utf8_lossy(&broken.stderr)
    );

    reexec::run(TEST, PHASE, None, |command| {
        command
            .env("APPIMAGE", fixture.path().join("coddy-studio.AppImage"))
            .env("APPDIR", &appdir)
            .env("LD_LIBRARY_PATH", &library_path)
            .env("CODDY_STUDIO_TEST_GIT_HELPER", &helper)
            .env(
                "CODDY_STUDIO_TEST_BUNDLED_LIBRARY",
                bundled.join(curl_library),
            )
            .env("CODDY_STUDIO_TEST_CUSTOM_LIBRARY_PATH", &custom)
            .env("SSH_AUTH_SOCK", fixture.path().join("agent.sock"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
    });
}

async fn assert_git_children() {
    let original = std::env::var_os("LD_LIBRARY_PATH").expect("inherited loader path");
    let appdir = std::env::var_os("APPDIR").expect("AppImage root");
    let helper = std::env::var_os("CODDY_STUDIO_TEST_GIT_HELPER").expect("HTTPS helper path");
    let custom =
        std::env::var("CODDY_STUDIO_TEST_CUSTOM_LIBRARY_PATH").expect("custom library path");
    let cancellation = CancellationToken::new();
    let mut https_request = request(&helper, &HELPER_ARGS);
    // Valid arguments force curl initialization even in older Git versions.
    // The capability query needs no HTTP request or external credentials.
    https_request.stdin = Some(b"capabilities\n\n".to_vec());

    let text = ProcessRunner
        .run(https_request.clone(), &cancellation)
        .await
        .expect("text runner");
    assert_eq!(
        text.exit_code, 0,
        "HTTPS helper must initialize and report capabilities: {}",
        text.stderr
    );
    assert!(text.stdout.lines().any(|line| line == "fetch"));
    let bytes = ProcessRunner
        .run_bytes(https_request.clone(), &cancellation)
        .await
        .expect("binary runner");
    assert_eq!(
        bytes.exit_code,
        0,
        "binary runner must use host libraries: {}",
        String::from_utf8_lossy(&bytes.stderr)
    );

    // Preserve a custom library directory whose name merely shares APPDIR's
    // prefix and credentials in both processes. Launcher markers belong only
    // to the parent, so APPDIR must be absent from the child.
    let echo = request(
        "/bin/sh",
        &[
            "-c",
            "printf '%s\\n' \"$LD_LIBRARY_PATH\" \"${APPDIR+present}\" \"$SSH_AUTH_SOCK\"",
        ],
    );
    let output = ProcessRunner
        .run(echo, &cancellation)
        .await
        .expect("environment probe");
    assert_eq!(
        output.stdout,
        format!(
            "{custom}\n\n{}\n",
            std::env::var("SSH_AUTH_SOCK").expect("agent socket")
        )
    );

    let mut semicolon = request("/bin/sh", &["-c", "printf '%s' \"$LD_LIBRARY_PATH\""]);
    semicolon.env.push((
        "LD_LIBRARY_PATH".into(),
        format!("{}/usr/lib;{custom}", appdir.to_string_lossy()).into(),
    ));
    let output = ProcessRunner
        .run(semicolon, &cancellation)
        .await
        .expect("semicolon-separated Linux library path");
    assert_eq!(output.stdout, custom);

    let mut only_bundled = request(
        "/bin/sh",
        &["-c", "printf '%s' \"${LD_LIBRARY_PATH+present}\""],
    );
    only_bundled.env.push((
        "LD_LIBRARY_PATH".into(),
        Path::new(&appdir).join("usr/lib").into_os_string(),
    ));
    let output = ProcessRunner
        .run_bytes(only_bundled, &cancellation)
        .await
        .expect("fully removed loader path");
    assert!(
        output.stdout.is_empty(),
        "remove the variable when no host entries remain"
    );

    let mut ordinary = request("/bin/sh", &["-c", "printf '%s' \"$LD_LIBRARY_PATH\""]);
    ordinary.env.push(("APPIMAGE".into(), "".into()));
    let output = ProcessRunner
        .run(ordinary, &cancellation)
        .await
        .expect("non-AppImage environment");
    assert_eq!(output.stdout, original.to_string_lossy());

    // AppRun appends ':' even when the original value is empty. A sole empty
    // survivor must not become '.', which would allow repository libraries to
    // load. The actual loader must ignore a planted cwd library when stripping
    // leaves no nonempty host entry.
    let library = PathBuf::from(
        std::env::var_os("CODDY_STUDIO_TEST_BUNDLED_LIBRARY").expect("bundled fixture library"),
    );
    fs::copy(
        &library,
        Path::new(&custom).join(library.file_name().expect("library name")),
    )
    .expect("working-directory library fixture");
    let mut cwd_library = https_request;
    cwd_library.cwd = PathBuf::from(&custom);
    cwd_library.env.push((
        "LD_LIBRARY_PATH".into(),
        format!("{}/usr/lib:", appdir.to_string_lossy()).into(),
    ));
    let output = ProcessRunner
        .run(cwd_library, &cancellation)
        .await
        .expect("working-directory libraries stay isolated");
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(output.stdout.lines().any(|line| line == "fetch"));
    assert!(
        !output.stderr.contains("symbol lookup error"),
        "a planted cwd library was loaded: {}",
        output.stderr
    );

    assert_eq!(std::env::var_os("LD_LIBRARY_PATH"), Some(original));
    assert_eq!(std::env::var_os("APPDIR"), Some(appdir));
}
