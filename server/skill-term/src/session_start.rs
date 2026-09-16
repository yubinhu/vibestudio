//! Validate a new pane's directory before starting an agent. tmux may silently
//! fall back to home when chdir fails, and its long-lived server can retain a
//! stale cwd. A successful new-window command alone is not a readiness signal.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) fn resolve_directory(input: &str) -> Result<String, String> {
    let path = if input.trim().is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        skill_core::pathsafe::resolve_root(input)
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("The host's working directory is unavailable: {error}. Choose an absolute session folder."))?
            .join(path)
    };
    // Keep the selected spelling for metadata/resume, including symlinks, but
    // validate against the real filesystem rather than tmux's fallback behavior.
    let real =
        fs::canonicalize(&path).map_err(|error| directory_error(&path, &error.to_string()))?;
    if !real.is_dir() {
        return Err(format!(
            "The session folder is not a directory: {}",
            path.display()
        ));
    }
    fs::read_dir(&path).map_err(|error| directory_error(&path, &error.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}

fn directory_error(path: &Path, detail: &str) -> String {
    let message = format!("Cannot start a session in “{}”: {detail}. Choose an existing folder that this host can read.", path.display());
    #[cfg(target_os = "macos")]
    let message = format!("{message} If macOS denied access, allow VibeStudio (or the terminal running skill-server) in System Settings → Privacy & Security → Files & Folders, then retry.");
    message
}

pub(super) struct Startup {
    dir: PathBuf,
}

impl Startup {
    pub(super) fn new() -> Result<Self, String> {
        let dir =
            std::env::temp_dir().join(format!("vibestudio-session-start-{}", super::new_uuid()));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|error| format!("Could not prepare the new session: {error}"))?;
        Ok(Self { dir })
    }

    pub(super) fn shell_prefix(&self, cwd: &str) -> String {
        self.directory_check(cwd, true)
    }

    pub(super) fn bootstrap(&self, cwd: &str, line: &str) -> String {
        // The login profile must still run in the selected project (it may
        // load a relative environment). Enter it from a clean POSIX shell,
        // then check again after the profile before announcing agent readiness.
        format!(
            "{}exec bash -lc {}",
            self.directory_check(cwd, false),
            super::shell_quote(line)
        )
    }

    fn directory_check(&self, cwd: &str, ready: bool) -> String {
        let cwd = super::shell_quote(cwd);
        let status = super::shell_quote(&self.dir.join("status").to_string_lossy());
        let error = super::shell_quote(&self.dir.join("error").to_string_lossy());
        // Re-enter by absolute path after login scripts, which may change cwd.
        // pwd exercises actual cwd resolution: chdir/stat can succeed even when
        // macOS refuses getcwd on a stale or protected directory. Never launch
        // either agent in tmux's fallback folder when this check fails.
        let success = if ready {
            format!("command printf 'ready\\n' >{status} || exit 1")
        } else {
            ":".into()
        };
        // This is our private IPC file; the login profile may enable noclobber
        // between the bootstrap and final check, so explicitly allow truncation.
        format!("if {{ command cd / && command cd -- {cwd} && /bin/pwd -P >/dev/null; }} 2>|{error}; then {success}; else command printf 'error\\n' >{status}; exit 1; fi; ")
    }

    pub(super) fn wait(&self, cwd: &str) -> Result<(), String> {
        self.wait_until(cwd, Instant::now() + Duration::from_secs(15))
    }

    fn wait_until(&self, cwd: &str, deadline: Instant) -> Result<(), String> {
        loop {
            match fs::read_to_string(self.dir.join("status"))
                .as_deref()
                .map(str::trim)
            {
                Ok("ready") => return Ok(()),
                Ok("error") => {
                    let mut detail = String::new();
                    if let Ok(file) = fs::File::open(self.dir.join("error")) {
                        let _ = file.take(4096).read_to_string(&mut detail);
                    }
                    return Err(directory_error(
                        Path::new(cwd),
                        if detail.trim().is_empty() {
                            "the new terminal could not enter or resolve this directory"
                        } else {
                            detail.trim()
                        },
                    ));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(format!("The new terminal did not finish checking “{cwd}”. Check that your login-shell startup files finish without waiting for input, then retry."));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_paths_and_files_before_creating_a_terminal() {
        let fixture = Startup::new().unwrap();
        let missing = fixture.dir.join("missing");
        assert!(resolve_directory(missing.to_str().unwrap())
            .unwrap_err()
            .contains("Cannot start a session"));
        let file = fixture.dir.join("file");
        fs::write(&file, "fixture").unwrap();
        assert!(resolve_directory(file.to_str().unwrap())
            .unwrap_err()
            .contains("not a directory"));
    }

    #[test]
    fn restores_selected_directory_after_shell_startup_and_preserves_symlink_metadata() {
        let fixture = Startup::new().unwrap();
        let selected = fixture.dir.join("project ' with spaces");
        fs::create_dir(&selected).unwrap();
        let link = fixture.dir.join("link");
        std::os::unix::fs::symlink(&selected, &link).unwrap();
        let resolved = resolve_directory(link.to_str().unwrap()).unwrap();
        assert_eq!(resolved, link.to_string_lossy());
        let startup = Startup::new().unwrap();
        let script = format!(
            "cd /; {} printf agent > started",
            startup.shell_prefix(&resolved)
        );
        let output = skill_core::process::hidden_command("bash")
            .env_remove("BASH_ENV")
            .args(["--noprofile", "--norc", "-c", &script])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        startup.wait(&resolved).unwrap();
        assert_eq!(
            fs::read_to_string(selected.join("started")).unwrap(),
            "agent"
        );
    }

    #[test]
    fn directory_removed_after_preflight_never_launches_agent_in_fallback_folder() {
        let fixture = Startup::new().unwrap();
        let selected = fixture.dir.join("removed");
        fs::create_dir(&selected).unwrap();
        let resolved = resolve_directory(selected.to_str().unwrap()).unwrap();
        fs::remove_dir(&selected).unwrap();
        let startup = Startup::new().unwrap();
        let marker = fixture.dir.join("must-not-start");
        let script = format!(
            "{} printf agent > {}",
            startup.shell_prefix(&resolved),
            super::super::shell_quote(marker.to_str().unwrap())
        );
        let output = skill_core::process::hidden_command("bash")
            .env_remove("BASH_ENV")
            .current_dir(&fixture.dir)
            .args(["--noprofile", "--norc", "-c", &script])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = startup.wait(&resolved).unwrap_err();
        assert!(error.contains(&resolved));
        assert!(
            !marker.exists(),
            "agent must never run in tmux's fallback folder"
        );
    }

    #[test]
    fn startup_timeout_is_bounded_and_removes_its_private_files() {
        let startup = Startup::new().unwrap();
        let path = startup.dir.clone();
        assert!(startup
            .wait_until("/fixture", Instant::now())
            .unwrap_err()
            .contains("startup files"));
        drop(startup);
        assert!(!path.exists());
    }
}
