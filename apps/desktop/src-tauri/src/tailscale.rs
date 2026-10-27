use coddy_studio_server::process::{configure_background_command, isolate_appimage_environment};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

const DEFAULT_TAILSCALE_SERVE_PORT: u16 = 443;
const TAILSCALE_STATUS_TIMEOUT: Duration = Duration::from_millis(1_500);
const TAILSCALE_PROBE_TIMEOUT: Duration = Duration::from_millis(2_500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleStatus {
    pub magic_dns_name: Option<String>,
    pub tailnet_ipv4_addresses: Vec<String>,
}

fn tailscale_command_candidates_for(platform: &str) -> Vec<PathBuf> {
    match platform {
        "windows" => vec![
            PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Tailscale\tailscale.exe"),
            PathBuf::from("tailscale.exe"),
        ],
        "macos" => vec![
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
            PathBuf::from("/opt/homebrew/bin/tailscale"),
            PathBuf::from("/usr/local/bin/tailscale"),
            PathBuf::from("tailscale"),
        ],
        _ => vec![
            PathBuf::from("/usr/bin/tailscale"),
            PathBuf::from("/usr/local/bin/tailscale"),
            PathBuf::from("/snap/bin/tailscale"),
            PathBuf::from("tailscale"),
        ],
    }
}

fn tailscale_command_candidates() -> Vec<PathBuf> {
    tailscale_command_candidates_for(std::env::consts::OS)
}

fn normalize_magic_dns_name(status: &Value) -> Option<String> {
    let normalized = status
        .get("Self")?
        .get("DNSName")?
        .as_str()?
        .trim()
        .trim_end_matches('.')
        .to_string();
    (!normalized.is_empty()).then_some(normalized)
}

pub fn is_tailscale_ipv4_address(address: &str) -> bool {
    let parts = address.split('.').collect::<Vec<_>>();
    if parts.len() != 4 {
        return false;
    }

    let Some(first) = parts.first().and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(second) = parts.get(1).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(_third) = parts.get(2).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(_fourth) = parts.get(3).and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };

    first == 100 && (64..=127).contains(&second)
}

pub fn parse_tailscale_status(raw_status_json: &str) -> Result<TailscaleStatus, String> {
    let status = serde_json::from_str::<Value>(raw_status_json)
        .map_err(|error| format!("Failed to decode tailscale status JSON: {error}"))?;
    let tailnet_ipv4_addresses = status
        .get("Self")
        .and_then(|self_value| self_value.get("TailscaleIPs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|address| is_tailscale_ipv4_address(address))
        .map(str::to_string)
        .collect::<Vec<_>>();

    Ok(TailscaleStatus {
        magic_dns_name: normalize_magic_dns_name(&status),
        tailnet_ipv4_addresses,
    })
}

pub fn build_tailscale_https_base_url(
    magic_dns_name: &str,
    serve_port: u16,
) -> Result<String, String> {
    let mut url = url::Url::parse(&format!("https://{magic_dns_name}"))
        .map_err(|error| format!("Could not build Tailscale HTTPS URL: {error}"))?;
    if serve_port != DEFAULT_TAILSCALE_SERVE_PORT {
        url.set_port(Some(serve_port))
            .map_err(|_| "Could not set Tailscale HTTPS port.".to_string())?;
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub async fn read_tailscale_status() -> Result<TailscaleStatus, String> {
    let mut failures = Vec::new();
    for candidate in tailscale_command_candidates() {
        match read_tailscale_status_with(&candidate, TAILSCALE_STATUS_TIMEOUT).await {
            Ok(status) => return Ok(status),
            Err(error) => failures.push(format!("{}: {error}", candidate.display())),
        }
    }
    Err(format!(
        "Could not read Tailscale status from any known CLI location: {}",
        failures.join("; ")
    ))
}

async fn read_tailscale_status_with(
    command_path: &Path,
    timeout: Duration,
) -> Result<TailscaleStatus, String> {
    let mut command = Command::new(command_path);
    configure_background_command(&mut command);
    command.kill_on_drop(true);
    isolate_appimage_environment(&mut command);
    let child = command
        .args(["status", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Failed to spawn tailscale status: {error}"))?;

    let output = wait_for_tailscale_output(child, timeout).await?;

    decode_tailscale_status_output(output)
}

fn decode_tailscale_status_output(output: std::process::Output) -> Result<TailscaleStatus, String> {
    if !output.status.success() {
        return Err(format!(
            "tailscale status exited with code {}.",
            output
                .status
                .code()
                .map_or_else(|| "unknown".to_string(), |code| code.to_string())
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("tailscale status returned non-UTF-8 JSON: {error}"))?;
    parse_tailscale_status(&stdout)
}

async fn wait_for_tailscale_output(
    mut child: Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut stdout = child
        .stdout
        .take()
        .expect("tailscale status stdout must remain piped");
    let mut stderr = child
        .stderr
        .take()
        .expect("tailscale status stderr must remain piped");
    let stdout_reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let stderr_reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => {
            result.map_err(|error| format!("Failed to read tailscale status output: {error}"))?
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = stdout_reader.await;
            let _ = stderr_reader.await;
            return Err(format!(
                "tailscale status timed out after {}ms.",
                timeout.as_millis()
            ));
        }
    };
    let stdout = stdout_reader
        .await
        .map_err(|error| format!("Failed to join tailscale stdout reader: {error}"))?
        .map_err(|error| format!("Failed to read tailscale status stdout: {error}"))?;
    let stderr = stderr_reader
        .await
        .map_err(|error| format!("Failed to join tailscale stderr reader: {error}"))?
        .map_err(|error| format!("Failed to read tailscale status stderr: {error}"))?;

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

pub async fn probe_tailscale_https_endpoint(base_url: &str) -> bool {
    let mut url = match url::Url::parse(base_url) {
        Ok(url) => url,
        Err(_) => return false,
    };
    url.set_path("/.well-known/coddy-studio/environment");
    url.set_query(None);
    url.set_fragment(None);

    let client = match reqwest::Client::builder()
        .timeout(TAILSCALE_PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    matches!(
        tokio::time::timeout(TAILSCALE_PROBE_TIMEOUT, client.get(url).send()).await,
        Ok(Ok(response)) if response.status().is_success()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::fs;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(code as u32)
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TAILSCALE_STATUS_JSON: &str = r#"{"Self":{"DNSName":"desktop.tail.ts.net.","TailscaleIPs":["100.100.100.100","fd7a:115c:a1e0::1","192.168.1.20"]}}"#;

    #[test]
    fn absolute_tailscale_candidates_precede_path_fallback_on_each_platform() {
        let windows = tailscale_command_candidates_for("windows");
        assert!(windows[0].to_string_lossy().starts_with(r"C:\"));
        assert_eq!(windows.last(), Some(&PathBuf::from("tailscale.exe")));

        let macos = tailscale_command_candidates_for("macos");
        assert_eq!(
            macos.first(),
            Some(&PathBuf::from(
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
            ))
        );
        assert_eq!(macos.last(), Some(&PathBuf::from("tailscale")));

        let linux = tailscale_command_candidates_for("linux");
        assert_eq!(linux.first(), Some(&PathBuf::from("/usr/bin/tailscale")));
        assert_eq!(linux.last(), Some(&PathBuf::from("tailscale")));
        assert!(
            windows[..windows.len() - 1]
                .iter()
                .all(|candidate| candidate.to_string_lossy().starts_with(r"C:\"))
        );
        assert!(
            macos[..macos.len() - 1]
                .iter()
                .chain(&linux[..linux.len() - 1])
                .all(|candidate| candidate.to_string_lossy().starts_with('/'))
        );
    }

    #[test]
    fn detects_tailnet_ipv4_addresses() {
        assert!(is_tailscale_ipv4_address("100.64.0.1"));
        assert!(is_tailscale_ipv4_address("100.127.255.254"));
        assert!(!is_tailscale_ipv4_address("100.128.0.1"));
        assert!(!is_tailscale_ipv4_address("192.168.1.44"));
        assert!(!is_tailscale_ipv4_address("not-an-ip"));
        assert!(!is_tailscale_ipv4_address("nope.64.0.1"));
        assert!(!is_tailscale_ipv4_address("100.nope.0.1"));
        assert!(!is_tailscale_ipv4_address("100.64.nope.1"));
        assert!(!is_tailscale_ipv4_address("100.64.0.nope"));
        assert!(!is_tailscale_ipv4_address("100.64.0.256"));
    }

    #[test]
    fn parses_status_facts() {
        let status = parse_tailscale_status(TAILSCALE_STATUS_JSON).expect("status should parse");

        assert_eq!(
            status.magic_dns_name,
            Some("desktop.tail.ts.net".to_string())
        );
        assert_eq!(
            status.tailnet_ipv4_addresses,
            vec!["100.100.100.100".to_string()]
        );

        assert_eq!(
            parse_tailscale_status(r#"{"Self":{"DNSName":" ... ","TailscaleIPs":[null,42]}}"#)
                .unwrap(),
            TailscaleStatus {
                magic_dns_name: None,
                tailnet_ipv4_addresses: Vec::new(),
            }
        );
        assert_eq!(
            parse_tailscale_status("{}").unwrap(),
            TailscaleStatus {
                magic_dns_name: None,
                tailnet_ipv4_addresses: Vec::new(),
            }
        );
        assert!(
            parse_tailscale_status("not json")
                .unwrap_err()
                .contains("decode")
        );
    }

    #[test]
    fn builds_clean_https_base_urls() {
        assert_eq!(
            build_tailscale_https_base_url("desktop.tail.ts.net", 443).expect("url should build"),
            "https://desktop.tail.ts.net/"
        );
        assert_eq!(
            build_tailscale_https_base_url("desktop.tail.ts.net", 8443).expect("url should build"),
            "https://desktop.tail.ts.net:8443/"
        );
        assert!(build_tailscale_https_base_url("desktop:invalid-port", 443).is_err());
    }

    #[tokio::test]
    async fn command_and_probe_helpers_reject_invalid_endpoints_without_io() {
        let fallback = tailscale_command_candidates()
            .pop()
            .expect("PATH fallback candidate");
        assert_eq!(
            fallback,
            PathBuf::from(if cfg!(target_os = "windows") {
                "tailscale.exe"
            } else {
                "tailscale"
            })
        );
        assert!(!probe_tailscale_https_endpoint("not a URL").await);
    }

    async fn probe_local_endpoint(status: &str) -> bool {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response_status = status.to_owned();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 1024];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /.well-known/coddy-studio/environment "));
            stream
                .write_all(
                    format!("HTTP/1.1 {response_status}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
        });

        let result =
            probe_tailscale_https_endpoint(&format!("http://{address}/ignored?query=yes")).await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn probe_reports_successful_and_failed_http_responses() {
        assert!(probe_local_endpoint("204 No Content").await);
        assert!(!probe_local_endpoint("503 Service Unavailable").await);
    }

    fn executable_script(
        directory: &Path,
        name: &str,
        unix_body: &str,
        windows_body: &str,
    ) -> PathBuf {
        #[cfg(unix)]
        {
            let _ = windows_body;
            let path = directory.join(name);
            crate::test_support::write_executable_fixture(
                &path,
                &format!("#!/bin/sh\n{unix_body}\n"),
                0o755,
            );
            path
        }
        #[cfg(windows)]
        {
            let _ = unix_body;
            let path = directory.join(format!("{name}.cmd"));
            fs::write(&path, format!("@echo off\r\n{windows_body}\r\n")).unwrap();
            path
        }
    }

    #[tokio::test]
    async fn status_command_reports_success_and_process_failures() {
        let directory = tempfile::tempdir().unwrap();
        let success = executable_script(
            directory.path(),
            "success",
            &format!("printf '%s' '{TAILSCALE_STATUS_JSON}'"),
            &format!("echo {TAILSCALE_STATUS_JSON}"),
        );
        assert_eq!(
            read_tailscale_status_with(&success, Duration::from_secs(15))
                .await
                .unwrap()
                .magic_dns_name
                .as_deref(),
            Some("desktop.tail.ts.net")
        );

        assert_eq!(
            decode_tailscale_status_output(std::process::Output {
                status: exit_status(7),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .unwrap_err(),
            "tailscale status exited with code 7."
        );

        assert!(
            decode_tailscale_status_output(std::process::Output {
                status: exit_status(0),
                stdout: vec![0xff],
                stderr: Vec::new(),
            })
            .unwrap_err()
            .contains("non-UTF-8")
        );

        assert!(
            parse_tailscale_status("nope")
                .unwrap_err()
                .contains("decode")
        );

        let slow = executable_script(
            directory.path(),
            "slow",
            "sleep 1",
            "%SystemRoot%\\System32\\ping.exe -n 2 127.0.0.1 >nul",
        );
        assert_eq!(
            read_tailscale_status_with(&slow, Duration::from_millis(10))
                .await
                .unwrap_err(),
            "tailscale status timed out after 10ms."
        );

        assert!(
            read_tailscale_status_with(&directory.path().join("missing"), Duration::from_secs(1))
                .await
                .unwrap_err()
                .contains("Failed to spawn")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn status_command_ignores_appimage_environment() {
        crate::test_support::with_appimage_test_environment_async(
            "tailscale::tests::status_command_ignores_appimage_environment",
            async {
                let directory = tempfile::tempdir().expect("Tailscale fixture directory");
                let executable = executable_script(
                    directory.path(),
                    "tailscale",
                    &format!(
                        r#"[ "$1" = status ] && [ "$2" = --json ] || exit 80
[ -z "${{APPIMAGE+x}}" ] && [ -z "${{PYTHONHOME+x}}" ] || exit 81
[ "$LD_LIBRARY_PATH" = /usr/lib ] || exit 82
[ "$CODDY_STUDIO_FUTURE_PATH" = /opt/host/future ] || exit 83
[ "$SSH_AUTH_SOCK" = /run/user/1000/coddy-studio-test-agent.sock ] || exit 84
printf '%s' '{TAILSCALE_STATUS_JSON}'"#
                    ),
                    "",
                );

                let status = read_tailscale_status_with(&executable, Duration::from_secs(15))
                    .await
                    .expect("Tailscale status must run with the host environment");

                assert_eq!(
                    status.magic_dns_name.as_deref(),
                    Some("desktop.tail.ts.net")
                );
            },
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_status_child_is_killed_and_reaped_before_return() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 60"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("status fixture child should start");
        let pid = child.id().expect("status fixture child should have a pid");

        let error = wait_for_tailscale_output(child, Duration::from_millis(10))
            .await
            .expect_err("status fixture child should time out");

        assert!(error.contains("timed out after 10ms"), "{error}");
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "timed-out status child must be reaped before returning"
        );
    }
}
