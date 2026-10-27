use std::{
    io::{Read, Write},
    thread,
    time::Duration,
};

use xpty::{CommandBuilder, PtySize, PtySystem, native_pty_system};

fn main() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut command = CommandBuilder::new("cmd.exe");
    command.args(["/d", "/c", "echo CODDY_STUDIO_PTY_READY"]);
    let mut child = pair.slave.spawn_command(command).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let reader_thread = thread::spawn(move || {
        let mut bytes = [0u8; 8192];
        let read = reader.read(&mut bytes).expect("read");
        String::from_utf8_lossy(&bytes[..read]).into_owned()
    });
    thread::sleep(Duration::from_millis(500));
    writer.flush().expect("flush");
    let status = child.wait().expect("wait");
    println!(
        "status={status:?} output={:?}",
        reader_thread.join().expect("reader join")
    );
}
