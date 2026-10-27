#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::{collections::BTreeMap, time::Duration};

use coddy_studio_server::terminal::{PortablePtyBackend, PtyBackend, PtySpawnInput};

const OUTPUT_MARKER: &str = "CODDY_STUDIO_GUI_PTY_OUTPUT";

#[tokio::test]
async fn gui_desktop_keeps_batch_terminal_attached_to_embedded_pty() {
    let directory = tempfile::tempdir().expect("terminal fixture directory");
    let script = directory.path().join("provider.cmd");
    std::fs::write(&script, format!("@echo off\r\necho {OUTPUT_MARKER}\r\n"))
        .expect("write terminal fixture");

    let process = PortablePtyBackend
        .spawn(&PtySpawnInput {
            executable: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        })
        .expect("GUI desktop PTY fixture should start");
    let mut output = process.subscribe_output();
    let mut exit = process.subscribe_exit();

    let captured = tokio::time::timeout(Duration::from_secs(10), async {
        let mut captured = String::new();
        while !captured.contains(OUTPUT_MARKER) {
            captured.push_str(
                &output
                    .recv()
                    .await
                    .expect("terminal output channel should stay open"),
            );
        }
        captured
    })
    .await;

    if captured.is_err() {
        process.kill().expect("timed-out terminal should stop");
    }
    let captured = captured.expect("batch output escaped the embedded PTY");

    tokio::time::timeout(Duration::from_secs(10), exit.changed())
        .await
        .expect("terminal should exit")
        .expect("terminal exit channel should stay open");
    assert_eq!(
        exit.borrow().as_ref().and_then(|event| event.exit_code),
        Some(0)
    );
    assert!(captured.contains(OUTPUT_MARKER));
}
