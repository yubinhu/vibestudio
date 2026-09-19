//! Live terminal context for file-link resolution; filesystem work lives in core.

use std::path::PathBuf;
use std::process::Command;

pub fn resolve_file_link(
    id: &str,
    path: &str,
) -> Result<skill_core::terminal_links::TerminalLinkFile, String> {
    let cwd = current_directory(id, super::tmux())?;
    skill_core::terminal_links::resolve_file(path, &cwd)
}

fn current_directory(id: &str, mut command: Command) -> Result<PathBuf, String> {
    if !super::valid_session_name(id) {
        return Err("Invalid terminal id.".into());
    }
    // '=' disables tmux's prefix/glob matching. The trailing ':' selects the
    // session's current window and active pane, even after `cd` or pane changes.
    let target = format!("={id}:");
    let output = command
        .args([
            "display-message",
            "-p",
            "-t",
            &target,
            "#{session_name}\n#{pane_current_path}",
        ])
        .output()
        .map_err(super::tmux_spawn_err)?;
    if !output.status.success() {
        return Err("That terminal session no longer exists.".into());
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| "The terminal's current directory is not valid UTF-8.")?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let (session, cwd) = text
        .split_once('\n')
        .ok_or("The terminal's current directory is unavailable.")?;
    if session != id
        || cwd.chars().any(char::is_control)
        || !std::path::Path::new(cwd).is_absolute()
    {
        return Err("The terminal's current directory is unavailable.".into());
    }
    Ok(PathBuf::from(cwd))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        id: String,
    }

    impl Fixture {
        fn new() -> Option<Self> {
            if super::super::tmux().arg("-V").output().is_err() {
                eprintln!("tmux unavailable — skipping live terminal link test");
                return None;
            }
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = PathBuf::from(format!("/tmp/vs-link-{}-{stamp}", std::process::id()));
            fs::create_dir_all(root.join("first")).unwrap();
            fs::create_dir_all(root.join("second ' folder")).unwrap();
            let fixture = Self {
                root,
                id: format!("ass-{}-{stamp}-10", std::process::id()),
            };
            fixture.run(&[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                &fixture.id,
                "-c",
                fixture.root.join("first").to_str().unwrap(),
                "bash",
                "--noprofile",
                "--norc",
                "-i",
            ]);
            Some(fixture)
        }

        fn command(&self) -> Command {
            let mut command = super::super::tmux();
            command.arg("-S").arg(self.root.join("socket"));
            command
        }

        fn run(&self, args: &[&str]) {
            let output = self.command().args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn cwd(&self) -> PathBuf {
            current_directory(&self.id, self.command()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.command().arg("kill-server").output();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn links_follow_live_cd_and_active_pane_with_exact_session_matching() {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let first = fs::canonicalize(fixture.root.join("first")).unwrap();
        let second = fs::canonicalize(fixture.root.join("second ' folder")).unwrap();
        fs::write(first.join("same.rs"), "first").unwrap();
        fs::write(second.join("same.rs"), "second").unwrap();
        assert_eq!(fixture.cwd(), first);
        assert_eq!(
            skill_core::terminal_links::resolve_file("same.rs", &fixture.cwd())
                .unwrap()
                .root,
            first.to_str().unwrap()
        );

        // The command itself is test input to a shell. The production resolver
        // only runs display-message with fixed format and validated session ID.
        fixture.run(&[
            "send-keys",
            "-t",
            &fixture.id,
            &format!(
                "cd -- {}",
                super::super::shell_quote(second.to_str().unwrap())
            ),
            "Enter",
        ]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.cwd() != second {
            assert!(Instant::now() < deadline, "pane did not change directory");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            skill_core::terminal_links::resolve_file("same.rs", &fixture.cwd())
                .unwrap()
                .root,
            second.to_str().unwrap()
        );

        fixture.run(&[
            "split-window",
            "-t",
            &fixture.id,
            "-c",
            first.to_str().unwrap(),
            "bash",
            "--noprofile",
            "--norc",
            "-i",
        ]);
        assert_eq!(fixture.cwd(), first, "resolution follows the active pane");

        // Prefix matching would find the existing -10 session when asked for -1.
        let prefix = fixture.id.strip_suffix('0').unwrap();
        assert!(current_directory(prefix, fixture.command()).is_err());
        for id in [
            "user-session",
            "ass-1-2-3:0",
            "ass-1-2-*",
            "ass-1-2-3\n",
            "=ass-1-2-3",
        ] {
            assert!(
                current_directory(id, fixture.command()).is_err(),
                "accepted {id:?}"
            );
        }
    }
}
