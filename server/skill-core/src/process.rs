//! The one place a child process is constructed. Everything that shells out —
//! git, ssh, wsl.exe, nvidia-smi, the llama-server engine, tmux, agent
//! `--version` probes — goes through `hidden_command`, so it spawns with
//! CREATE_NO_WINDOW on Windows. A packaged GUI app has no console to reuse, so
//! without that flag each invocation flashes its own console window; a burst of
//! them on connect/startup reads as windows popping up everywhere. The flag is a
//! no-op off Windows. A clippy `disallowed-methods` rule (see `clippy.toml`)
//! turns the raw `Command::new` into an error so a new call site can't forget.
use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Construct a `Command` that won't flash a console window on Windows. Use this
/// in place of `Command::new` for every spawn (the lint enforces it).
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    // The single sanctioned `Command::new`: every other call site routes here.
    #[allow(clippy::disallowed_methods)]
    let mut cmd = Command::new(program);
    hide_window(&mut cmd);
    cmd
}

/// Apply the no-window flag to a command you already hold a builder for.
/// `hidden_command` is the usual entry point; reach for this only when the
/// `Command` was constructed elsewhere.
pub fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// A practical descriptor allowance for the host and the agents it starts.
/// Increasing the allowance does not open or reserve any descriptors. Keep it
/// finite: macOS can report an unlimited hard rlimit while enforcing a separate
/// kernel ceiling, and setting the soft limit to infinity fails on older macOS.
pub fn open_file_limit_target() -> u64 {
    let target = 8192;
    #[cfg(target_os = "macos")]
    {
        let mut ceiling: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&ceiling);
        // SAFETY: the name is NUL-terminated; the output buffer and size match.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&mut ceiling as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result == 0 && ceiling > 0 {
            return target.min(ceiling as u64);
        }
    }
    target
}

/// Lift an inherited low soft NOFILE limit once, before opening the server's
/// listeners or spawning tmux. Never lower either limit or raise the hard cap.
/// This is best-effort so a restrictive host policy does not prevent startup.
pub fn prepare_open_file_limit() {
    #[cfg(unix)]
    {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: getrlimit writes to a valid, correctly sized struct.
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
                log::warn!("could not read the open-file limit: {}", std::io::Error::last_os_error());
                return;
            }
            let original = limit.rlim_cur;
            let desired = open_file_limit_target() as libc::rlim_t;
            let target = desired.min(limit.rlim_max);
            if original < target {
                limit.rlim_cur = target;
                // SAFETY: this is a valid rlimit; its hard cap is unchanged.
                if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
                    log::warn!(
                        "could not raise the open-file soft limit from {original} to {target} (hard={}): {}",
                        limit.rlim_max, std::io::Error::last_os_error()
                    );
                    return;
                }
                log::info!("raised open-file soft limit from {original} to {target} (hard={})", limit.rlim_max);
            }
            if limit.rlim_cur < desired {
                log::warn!(
                    "open-file soft limit is {} (hard={}); agents may exhaust this host's descriptor allowance",
                    limit.rlim_cur, limit.rlim_max
                );
            }
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn limits() -> libc::rlimit {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit
    }

    #[test]
    fn open_file_limit_preserves_hard_cap_and_existing_higher_soft_limit() {
        const PROBE: &str = "VIBESTUDIO_TEST_OPEN_FILE_LIMIT";
        if std::env::var_os(PROBE).is_some() {
            let before = limits();
            prepare_open_file_limit();
            let after = limits();
            assert_eq!(after.rlim_max, before.rlim_max);
            assert_eq!(
                after.rlim_cur,
                before
                    .rlim_cur
                    .max((open_file_limit_target() as libc::rlim_t).min(before.rlim_max))
            );
            return;
        }
        // Only children lower their hard limits: the test runner and other
        // parallel tests retain their own resource allowances.
        let inherited = limits();
        for (soft, hard) in [(256, 8192), (256, 512), (256, 256), (9000, 9000)] {
            if inherited.rlim_max < hard
                || (open_file_limit_target() < 8192 && soft > open_file_limit_target())
            {
                continue;
            }
            let mut command = hidden_command(std::env::current_exe().unwrap());
            command.args([
                "--exact", "process::tests::open_file_limit_preserves_hard_cap_and_existing_higher_soft_limit",
                "--nocapture",
            ]).env(PROBE, "1");
            // SAFETY: setrlimit is the only child-side operation before exec.
            unsafe {
                command.pre_exec(move || {
                    let limit = libc::rlimit {
                        rlim_cur: soft,
                        rlim_max: hard,
                    };
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "soft={soft}, hard={hard}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
