#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::{path::Path, process::Stdio, time::Duration};

use coddy_studio_server::process::{
    configure_background_command, configure_supervised_background_command_wrap,
};
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use windows_sys::Win32::System::Console::GetConsoleWindow;

const CONSOLE_PROBE_CHILD_ENV: &str = "CODDY_STUDIO_TEST_GUI_CONSOLE_PROBE_CHILD";
const CONSOLE_PROBE_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);
const NO_CONSOLE_MARKER: &str = "no-console";

#[test]
fn gui_console_probe_child_fixture() {
    let Ok(marker_path) = std::env::var(CONSOLE_PROBE_CHILD_ENV) else {
        return;
    };
    // SAFETY: GetConsoleWindow takes no arguments and only reads this fixture
    // process's console association.
    let marker = if unsafe { GetConsoleWindow() }.is_null() {
        NO_CONSOLE_MARKER
    } else {
        "console"
    };
    std::fs::write(marker_path, marker).expect("console probe marker should be written");
    std::thread::sleep(Duration::from_secs(30));
}

#[tokio::test]
async fn direct_background_command_has_no_console_window_from_gui_parent() {
    let directory = tempfile::tempdir().expect("console probe temporary directory");
    let marker_path = directory.path().join("direct-console-probe.txt");
    let mut command = tokio::process::Command::new(
        std::env::current_exe().expect("current test executable should resolve"),
    );
    command
        .args(["gui_console_probe_child_fixture", "--exact", "--nocapture"])
        .env(CONSOLE_PROBE_CHILD_ENV, &marker_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_background_command(&mut command);
    let mut child = command
        .spawn()
        .expect("direct background console probe should start");

    assert_child_has_no_visible_console(&mut child, &marker_path).await;
}

#[tokio::test]
async fn supervised_background_command_has_no_console_window_from_gui_parent() {
    let directory = tempfile::tempdir().expect("console probe temporary directory");
    let marker_path = directory.path().join("supervised-console-probe.txt");
    let executable = std::env::current_exe().expect("current test executable should resolve");
    let mut command = CommandWrap::with_new(executable, |command| {
        command
            .args(["gui_console_probe_child_fixture", "--exact", "--nocapture"])
            .env(CONSOLE_PROBE_CHILD_ENV, &marker_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    });
    configure_supervised_background_command_wrap(&mut command);

    let mut child = command
        .spawn()
        .expect("background console probe should start");
    assert_child_has_no_visible_console(&mut *child, &marker_path).await;
}

async fn assert_child_has_no_visible_console(child: &mut dyn ChildWrapper, marker_path: &Path) {
    let observation = tokio::time::timeout(CONSOLE_PROBE_INTEGRATION_DEADLINE, async {
        loop {
            if let Ok(marker) = std::fs::read_to_string(marker_path)
                && matches!(marker.as_str(), NO_CONSOLE_MARKER | "console")
            {
                break marker;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;

    child
        .start_kill()
        .expect("background console probe should be terminated");
    child
        .wait()
        .await
        .expect("background console probe should exit");
    if marker_path.is_file() {
        std::fs::remove_file(marker_path).expect("console probe marker should be removed");
    }

    let observation = observation.expect("GUI child console probe should publish its observation");
    assert_eq!(observation, NO_CONSOLE_MARKER);
}
