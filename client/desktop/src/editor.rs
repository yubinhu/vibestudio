//! The shell's half of [`skill_server::EditorControl`] — the "Open in VS Code"
//! affordance on the Sessions page. Opening an editor acts on the machine whose
//! screen the user is at, so it lives HERE in the client shell, not in the
//! shippable `skill-core` backend (which also runs headless on a remote host and
//! must never try to pop a window). Reached only over the pinned-local
//! `/api/editor/*` route (never proxied), same one-way rule as `ShellNotifier`.
//!
//! When a remote is connected the folder lives on the remote, so we open it via
//! VS Code's matching remote extension: `ssh-remote+<host>` for SSH or
//! `wsl+<distro>` for a WSL connection. Both open a LOCAL editor window attached
//! to the selected host, instead of treating its path as a local `code <path>`.
//!
//! Locating `code` can't lean on PATH alone: a packaged desktop app is launched
//! from the dock/menu with a stripped login PATH (notably macOS — `/usr/bin:/bin`
//! only), so VS Code installed the usual way wouldn't be on it. We search PATH
//! first, then the well-known install locations per OS (including the CLI shim
//! inside the macOS `.app` bundle).
use std::path::{Path, PathBuf};
use std::process::Command;

use skill_core::process::hidden_command;

/// The VS Code CLIs we recognize, best first. Both are "VS Code".
const EDITORS: &[(&str, &str)] = &[("code", "VS Code"), ("code-insiders", "VS Code Insiders")];

/// The shell's editor control, injected into the loopback server as `ctx.editor`.
pub struct ShellEditor;

impl skill_server::EditorControl for ShellEditor {
    /// The reachable editor's display name (`Some` → the button shows), or `None`.
    fn detect(&self) -> Option<String> {
        locate().map(|(_, name)| name.to_string())
    }

    /// Launch VS Code on `path` (a session's working directory). Non-blocking — we
    /// spawn and return; the editor window is the user's feedback. When `remote_host`
    /// is set the path is on that remote, so use its SSH or WSL authority.
    fn open(&self, path: &str, remote_host: Option<&str>) -> Result<(), String> {
        let (bin, name) =
            locate().ok_or("VS Code (the `code` command) was not found on this machine")?;
        editor_command(&bin, path, remote_host)?
            .spawn()
            .map_err(|e| format!("failed to launch {name}: {e}"))?;
        Ok(())
    }
}

/// Shared by session folders and linked-file previews. Keep the path as one
/// literal CLI argument; spaces, Unicode and file-name punctuation are not URLs
/// and must not be percent-encoded or stripped from the supplied path.
fn editor_command(bin: &Path, path: &str, remote_host: Option<&str>) -> Result<Command, String> {
    if path.trim().is_empty() {
        return Err("no file or folder to open".into());
    }
    let mut command = spawn_command(bin);
    if let Some(host) = remote_host {
        // VS Code documents `code --remote wsl+<distro> <path in WSL>`:
        // https://code.visualstudio.com/docs/remote/wsl
        // VibeStudio's `wsl:` connection prefix is not an SSH hostname.
        let authority = match host.strip_prefix("wsl:") {
            Some("") => return Err("The WSL connection has no distribution name.".into()),
            Some(distro) => format!("wsl+{distro}"),
            None => format!("ssh-remote+{host}"),
        };
        command.arg("--remote").arg(authority);
    }
    command.arg(path);
    Ok(command)
}

/// The first recognized editor's resolved binary, with its display name.
fn locate() -> Option<(PathBuf, &'static str)> {
    for (cli, name) in EDITORS {
        if let Some(p) = resolve(cli) {
            return Some((p, name));
        }
    }
    None
}

/// PATH first (the common case), then the OS's well-known install locations.
fn resolve(cli: &str) -> Option<PathBuf> {
    search_path(cli).or_else(|| known_locations(cli).into_iter().find(|p| p.is_file()))
}

fn search_path(cli: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        exe_names(cli).into_iter().map(|n| dir.join(n)).find(|c| c.is_file())
    })
}

/// Candidate filenames for `cli` in a PATH dir. Windows ships `code.cmd` (a shim)
/// and `Code.exe`; elsewhere the bare name.
fn exe_names(cli: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![format!("{cli}.cmd"), format!("{cli}.exe"), cli.to_string()]
    }
    #[cfg(not(windows))]
    {
        vec![cli.to_string()]
    }
}

/// Well-known absolute install paths for `cli`, the fallback when PATH is stripped.
fn known_locations(cli: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        v.push(PathBuf::from("/usr/local/bin").join(cli));
        v.push(PathBuf::from("/opt/homebrew/bin").join(cli));
        // The CLI shim inside the .app — the reliable path for a dock-launched
        // app whose PATH is `/usr/bin:/bin` only.
        if cli == "code" {
            let app = "Visual Studio Code.app/Contents/Resources/app/bin/code";
            v.push(PathBuf::from("/Applications").join(app));
            if let Some(home) = std::env::var_os("HOME") {
                v.push(PathBuf::from(home).join("Applications").join(app));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for base in ["/usr/bin", "/usr/local/bin", "/snap/bin", "/var/lib/flatpak/exports/bin"] {
            v.push(PathBuf::from(base).join(cli));
        }
        if let Some(home) = std::env::var_os("HOME") {
            v.push(PathBuf::from(home).join(".local/bin").join(cli));
        }
    }

    #[cfg(windows)]
    {
        // Prefer the real Code.exe (a PE binary spawns directly); fall back to the
        // bin\*.cmd shim, launched via `cmd /C` in spawn_command.
        let exe = if cli == "code" { "Code.exe" } else { "Code - Insiders.exe" };
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var) {
                let root = PathBuf::from(base).join("Microsoft VS Code");
                v.push(root.join(exe));
                v.push(root.join("bin").join(format!("{cli}.cmd")));
            }
        }
    }

    v
}

/// A spawn builder for `bin`. On Windows a `.cmd`/`.bat` shim isn't a PE binary,
/// so it must be run through `cmd /C`; everything else spawns directly.
fn spawn_command(bin: &Path) -> Command {
    #[cfg(windows)]
    {
        if bin.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")) {
            let mut c = hidden_command("cmd");
            c.arg("/C").arg(bin);
            return c;
        }
    }
    hidden_command(bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(path: &str, host: Option<&str>) -> Vec<String> {
        editor_command(Path::new("code"), path, host).unwrap()
            .get_args().map(|arg| arg.to_str().unwrap().to_string()).collect()
    }

    #[test]
    fn folders_and_linked_files_use_the_selected_transport_authority() {
        for path in ["/home/harvey/repos/vibestudio", "/home/harvey/repos/vibestudio/plans/terminal-links-research.md"] {
            assert_eq!(args(path, None), [path]);
            assert_eq!(args(path, Some("workbox")), ["--remote", "ssh-remote+workbox", path]);
            assert_eq!(args(path, Some("user@workbox")), ["--remote", "ssh-remote+user@workbox", path]);
            assert_eq!(args(path, Some("wsl:Ubuntu")), ["--remote", "wsl+Ubuntu", path]);
            assert_eq!(args(path, Some("wsl:Ubuntu-24.04")), ["--remote", "wsl+Ubuntu-24.04", path]);
        }
    }

    #[test]
    fn editor_arguments_preserve_spaces_unicode_and_punctuation() {
        let path = "/home/harvey/Project notes/目录/[draft] a&b#1%HOME% 'quote' $(literal).md ";
        assert_eq!(args(path, Some("wsl:Ubuntu")), ["--remote", "wsl+Ubuntu", path]);
        assert!(editor_command(Path::new("code"), "   ", None).is_err());
        assert!(editor_command(Path::new("code"), path, Some("wsl:")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_editor_receives_literal_arguments_without_opening_an_editor() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vibestudio-editor-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let stub = dir.join("code");
        std::fs::write(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = "/home/harvey/Project notes/目录/[draft] a&b#1%HOME% 'quote' $(literal).md ";
        let result = editor_command(&stub, path, Some("wsl:Ubuntu")).unwrap().output().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, format!("--remote\0wsl+Ubuntu\0{path}\0").as_bytes());
    }
}
