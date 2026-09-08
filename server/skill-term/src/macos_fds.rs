//! Keep the detached tmux server from inheriting unrelated client pipes.
//!
//! macOS creates pipes and sets CLOEXEC in separate syscalls. A concurrent
//! spawn can inherit a pipe between those calls; if tmux daemonizes with its
//! write end, another command's output reader never receives EOF.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub(super) fn isolate(command: &mut Command) {
    // SAFETY: after fork this callback uses only a fixed stack buffer and
    // syscall wrappers; it does not allocate, log, or acquire a userspace lock.
    // Marking descriptors (instead of closing them immediately) preserves
    // Rust's exec-error pipe until exec and leaves redirected stdio intact.
    unsafe { command.pre_exec(mark_inherited_cloexec) };
}

fn mark_inherited_cloexec() -> io::Result<()> {
    let mut descriptors = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; 4096];
    let capacity = std::mem::size_of_val(&descriptors);
    // proc_pidinfo is a direct __proc_info syscall wrapper on macOS.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            descriptors.as_mut_ptr().cast(),
            capacity as i32,
        )
    };
    if bytes > 0 && (bytes as usize) < capacity {
        for descriptor in &descriptors[..bytes as usize / std::mem::size_of::<libc::proc_fdinfo>()]
        {
            mark(descriptor.proc_fd)?;
        }
        return Ok(());
    }

    // An unusual descriptor count or restricted proc introspection must still
    // isolate the child. The normal path only visits the open descriptors.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in 3..limit.rlim_cur.min(i32::MAX as libc::rlim_t) as i32 {
        mark(fd)?;
    }
    Ok(())
}

fn mark(fd: i32) -> io::Result<()> {
    if fd < 3 {
        return Ok(());
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::EBADF) {
            Ok(())
        } else {
            Err(error)
        };
    }
    if flags & libc::FD_CLOEXEC == 0
        && unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    #[test]
    fn child_does_not_inherit_other_commands_pipe() {
        const PROBE: &str = "VIBESTUDIO_TEST_INHERITED_PIPE";
        if let Ok(value) = std::env::var(PROBE) {
            let fd: i32 = value.parse().unwrap();
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            println!("child stdio still works");
            return;
        }
        let (reader, writer) = std::io::pipe().unwrap();
        // Reproduce the race deterministically: F_DUPFD deliberately leaves
        // CLOEXEC clear, just as the pipe is before its creator sets that flag.
        let raw = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD, 128) };
        assert!(raw >= 128);
        let leaked = unsafe { OwnedFd::from_raw_fd(raw) };
        assert_eq!(
            unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        let command = || {
            let mut command = skill_core::process::hidden_command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "macos_fds::tests::child_does_not_inherit_other_commands_pipe",
                    "--nocapture",
                ])
                .env(PROBE, raw.to_string());
            command
        };
        assert!(
            !command().output().unwrap().status.success(),
            "the unprotected child must expose the inherited descriptor"
        );
        let mut isolated = command();
        isolate(&mut isolated);
        let output = isolated.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("child stdio still works"));
        drop((reader, writer, leaked));

        let mut missing = skill_core::process::hidden_command("/nonexistent/vibestudio-fd-test");
        isolate(&mut missing);
        assert_eq!(missing.output().unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}
