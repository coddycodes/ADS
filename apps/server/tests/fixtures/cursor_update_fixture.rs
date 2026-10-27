use std::{
    env, fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process, thread,
    time::Duration,
};

const VERSION_PROBE_CONTROL_ENV: &str = "CODDY_STUDIO_TEST_CURSOR_VERSION_PROBE_CONTROL";

fn adjacent_file(name: &str) -> PathBuf {
    env::current_exe()
        .expect("Cursor fixture executable path")
        .parent()
        .expect("Cursor fixture release directory")
        .join(name)
}

fn read_trimmed(name: &str) -> String {
    fs::read_to_string(adjacent_file(name))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn configured_exit_code(name: &str) -> Option<i32> {
    read_trimmed(name)
        .parse::<i32>()
        .ok()
        .filter(|exit_code| *exit_code != 0)
}

fn reported_version(exit_code: Option<i32>) -> String {
    if exit_code.is_some() {
        read_trimmed("failed-version-state")
    } else {
        read_trimmed("version-state")
    }
}

fn pause_version_probe_if_configured() {
    let Some(control_address) = env::var_os(VERSION_PROBE_CONTROL_ENV) else {
        return;
    };
    let control_address = control_address
        .into_string()
        .expect("version probe control address is UTF-8");
    let mut control = TcpStream::connect(control_address).expect("connect version probe control");
    control
        .write_all(b"E")
        .expect("write version probe entered event");
    let mut release = [0_u8; 1];
    control
        .read_exact(&mut release)
        .expect("read version probe release event");
    assert_eq!(release, [b'R'], "unexpected version probe release event");
}

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--version") => {
            pause_version_probe_if_configured();
            let exit_code = configured_exit_code("version-exit-code");
            println!("cursor-agent {}", reported_version(exit_code));
            if let Some(exit_code) = exit_code {
                process::exit(exit_code);
            }
        }
        Some("about") => {
            let exit_code = configured_exit_code("about-exit-code");
            if let Some(exit_code) = exit_code {
                println!("grok-cli {}", reported_version(Some(exit_code)));
                process::exit(exit_code);
            }
            println!(
                r#"{{"cliVersion":"{}"}}"#,
                reported_version(None)
            );
        }
        Some("update") => {
            fs::write(adjacent_file("update-invoked"), b"invoked")
                .expect("write update marker");
            if let Ok(milliseconds) = read_trimmed("update-sleep-ms").parse::<u64>() {
                thread::sleep(Duration::from_millis(milliseconds));
            }
            if let Ok(exit_code) = read_trimmed("update-exit-code").parse::<i32>()
                && exit_code != 0
            {
                eprintln!("cursor update failed");
                process::exit(exit_code);
            }
            let next_version = read_trimmed("next-version");
            if !next_version.is_empty() {
                fs::write(adjacent_file("version-state"), next_version)
                    .expect("write updated Cursor version");
            }
            let _ = fs::remove_file(adjacent_file("version-exit-code"));
            let _ = fs::remove_file(adjacent_file("about-exit-code"));
            println!("cursor updated");
        }
        _ => process::exit(1),
    }
}
