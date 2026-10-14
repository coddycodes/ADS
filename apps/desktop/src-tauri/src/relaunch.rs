//! Keeps an AppImage relaunch from inheriting the old runtime's keep-alive pipe.

use std::sync::atomic::{AtomicBool, Ordering};

/// An installed update asked for a relaunch; read at the last exit callback.
static RELAUNCH_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Marks every descriptor above stderr close-on-exec before relaunch.
/// A no-op outside Linux; only exec'd children lose the descriptors.
pub(crate) fn prepare_descriptors_for_relaunch() {
    #[cfg(target_os = "linux")]
    linux::mark_inherited_descriptors_close_on_exec();
}

/// Called just before `AppHandle::restart` after an update installs.
pub(crate) fn note_relaunch_requested() {
    RELAUNCH_REQUESTED.store(true, Ordering::Release);
}

/// Whether this process's exit takes its AppImage mount down while its children still run
/// from it: a service unit's stop kills its whole cgroup, the mount's FUSE process included.
/// A crash monitor that ignored that SIGTERM then dies with SIGBUS on its next page fault.
pub(crate) fn exit_takes_mount_from_children() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("APPIMAGE").is_some()
            && linux::runs_in_service_unit(
                &std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default(),
            )
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// The relaunch Tauri performs after the last exit callback spawns the new app as a child
/// in this process's cgroup. When this process is a systemd service's main process (a
/// launcher such as `systemd-run --user`), its exit stops the unit, which kills that whole
/// cgroup: the new AppImage's mount goes away and the relaunched app dies with SIGBUS
/// (#94). Here the new app starts in its own transient scope instead, and this process
/// exits; when that is not needed or `systemd-run` fails, Tauri's relaunch runs as before.
pub(crate) fn relaunch_outside_service_unit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if !RELAUNCH_REQUESTED.load(Ordering::Acquire) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        if !linux::runs_in_service_unit(&cgroup) {
            return;
        }
        let env = tauri::Manager::env(app);
        let Ok(binary) = tauri::process::current_binary(&env) else {
            return;
        };
        let mut command = std::process::Command::new("systemd-run");
        command
            .args(["--user", "--scope", "--collect", "--quiet", "--"])
            .arg(binary)
            .args(env.args_os.iter().skip(1));
        bibcode_server::process::isolate_appimage_environment(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, "systemd-run is unavailable; relaunching in place");
                return;
            }
        };
        // Exit only once the new process has left this unit's cgroup: exiting earlier stops
        // the unit and could kill systemd-run before it moved into its scope.
        let cgroup_path = format!("/proc/{}/cgroup", child.id());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                tracing::warn!(%status, "systemd-run did not start the update's relaunch scope; relaunching in place");
                return;
            }
            let cgroup = std::fs::read_to_string(&cgroup_path).unwrap_or_default();
            if !cgroup.is_empty() && !linux::runs_in_service_unit(&cgroup) {
                tracing::info!("relaunching the updated app in its own systemd scope");
                std::process::exit(0);
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    "systemd-run did not leave the service unit in time; relaunching in place"
                );
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = app;
}

#[cfg(target_os = "linux")]
mod linux {
    const FIRST_INHERITED_DESCRIPTOR: libc::c_uint = 3;

    pub(super) fn mark_inherited_descriptors_close_on_exec() {
        // Use syscall so CLOSE_RANGE_CLOEXEC does not depend on the build host's glibc.
        let result = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                FIRST_INHERITED_DESCRIPTOR,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if result == 0 {
            return;
        }

        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
            tracing::warn!(%error, "close_range could not mark descriptors close-on-exec");
        }
        mark_through_proc();
    }

    /// Whether this process sits directly in a `.service` unit, whose stop kills its whole
    /// cgroup. Scopes (terminals, desktop launchers) end only when empty.
    pub(super) fn runs_in_service_unit(cgroup: &str) -> bool {
        cgroup
            .lines()
            .filter_map(|line| line.rsplit('/').next())
            .any(|unit| unit.trim().ends_with(".service"))
    }

    pub(super) fn mark_through_proc() {
        let entries = match std::fs::read_dir("/proc/self/fd") {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, "could not list descriptors before relaunch");
                return;
            }
        };
        // Collect first: the directory handle is itself a descriptor.
        let descriptors = entries
            .filter_map(|entry| match entry {
                Ok(entry) => entry.file_name().to_str()?.parse::<libc::c_int>().ok(),
                Err(error) => {
                    tracing::warn!(%error, "could not read a descriptor entry before relaunch");
                    None
                }
            })
            .filter(|fd| *fd >= FIRST_INHERITED_DESCRIPTOR as libc::c_int)
            .collect::<Vec<_>>();
        for fd in descriptors {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags == -1 {
                let error = std::io::Error::last_os_error();
                // A descriptor can close since the listing, including the directory handle.
                if error.raw_os_error() != Some(libc::EBADF) {
                    tracing::warn!(fd, %error, "could not read descriptor flags before relaunch");
                }
                continue;
            }
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
                let error = std::io::Error::last_os_error();
                tracing::warn!(fd, %error, "could not mark a descriptor close-on-exec");
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn only_a_service_unit_needs_its_own_relaunch_scope() {
        use super::linux::runs_in_service_unit;
        assert!(runs_in_service_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/bibcode-desktop.service\n"
        ));
        assert!(!runs_in_service_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-foot-1234.scope\n"
        ));
        assert!(!runs_in_service_unit(
            "0::/user.slice/user-1000.slice/session-2.scope\n"
        ));
        assert!(!runs_in_service_unit(""));
    }

    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Command;

    fn inherited_pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // A plain pipe(2) has no close-on-exec, like the runtime's keep-alive pipe.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: pipe returned two new descriptors, each with a single owner.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn has_cloexec(fd: libc::c_int) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags, -1, "F_GETFD failed");
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn an_inherited_pipe_is_marked_close_on_exec_and_a_spawned_child_lacks_it() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "relaunch::tests::an_inherited_pipe_is_marked_close_on_exec_and_a_spawned_child_lacks_it",
        ) else {
            return;
        };

        let (read_end, write_end) = inherited_pipe();
        assert!(!has_cloexec(read_end.as_raw_fd()));
        assert!(!has_cloexec(write_end.as_raw_fd()));
        assert!(!has_cloexec(std::io::stdin().as_raw_fd()));

        super::prepare_descriptors_for_relaunch();

        assert!(
            has_cloexec(read_end.as_raw_fd()),
            "read end must be close-on-exec"
        );
        assert!(
            has_cloexec(write_end.as_raw_fd()),
            "write end must be close-on-exec"
        );
        assert!(
            !has_cloexec(std::io::stdin().as_raw_fd()),
            "stdio stays inheritable"
        );

        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("test -e /proc/self/fd/{}", write_end.as_raw_fd()))
            .status()
            .expect("sh should spawn");
        assert!(!status.success(), "the child must not inherit the pipe");
        isolated.complete();
    }

    #[test]
    fn proc_fallback_marks_both_pipe_ends_close_on_exec() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "relaunch::tests::proc_fallback_marks_both_pipe_ends_close_on_exec",
        ) else {
            return;
        };

        let (read_end, write_end) = inherited_pipe();
        assert!(!has_cloexec(read_end.as_raw_fd()));
        assert!(!has_cloexec(write_end.as_raw_fd()));

        super::linux::mark_through_proc();

        assert!(
            has_cloexec(read_end.as_raw_fd()),
            "read end must be close-on-exec"
        );
        assert!(
            has_cloexec(write_end.as_raw_fd()),
            "write end must be close-on-exec"
        );
        assert!(
            !has_cloexec(std::io::stdin().as_raw_fd()),
            "stdio stays inheritable"
        );
        isolated.complete();
    }
}
