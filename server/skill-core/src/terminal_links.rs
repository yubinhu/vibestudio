//! Resolve terminal file links on the machine that owns the terminal.
//!
//! A terminal may reference any file its user can read, including paths outside
//! its original project. The canonical parent and basename form a normal file
//! editor root/relative-path pair without relaxing the editor's path guards.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TerminalLinkFile {
    pub path: String,
    pub root: String,
    pub rel: String,
}

/// `path` is a filesystem path, already separated from any line/column suffix
/// or file URI by the terminal UI. Never interpret it as a shell expression.
pub fn resolve_file(path: &str, cwd: &Path) -> Result<TerminalLinkFile, String> {
    resolve_with_home(path, cwd, dirs::home_dir().as_deref())
}

fn resolve_with_home(
    path: &str,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<TerminalLinkFile, String> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err("Invalid terminal file path.".into());
    }
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = home.ok_or("The terminal host's home directory is unavailable.")?;
        home.join(path.strip_prefix("~/").unwrap_or(""))
    } else if path.starts_with('~') {
        return Err("Only ~/ paths for the terminal host's user are supported.".into());
    } else {
        PathBuf::from(path)
    };
    let target = if expanded.is_absolute() {
        expanded
    } else {
        if !cwd.is_absolute() {
            return Err("The terminal's current directory is unavailable.".into());
        }
        cwd.join(expanded)
    };
    let target = std::fs::canonicalize(&target)
        .map_err(|_| format!("File not found on the terminal host: {path}"))?;
    if !target.is_file() {
        return Err(format!("The terminal link is not a regular file: {path}"));
    }
    let root = target
        .parent()
        .ok_or("The terminal file has no parent directory.")?;
    let rel = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("The terminal file path is not valid UTF-8.")?;
    Ok(TerminalLinkFile {
        path: target
            .to_str()
            .ok_or("The terminal file path is not valid UTF-8.")?
            .into(),
        root: root
            .to_str()
            .ok_or("The terminal file path is not valid UTF-8.")?
            .into(),
        rel: rel.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "vs-terminal-links-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("project/nested")).unwrap();
            fs::create_dir_all(root.join("home")).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_relative_parent_absolute_and_home_paths() {
        let fixture = Fixture::new();
        let project = fixture.0.join("project");
        let home = fixture.0.join("home");
        let file = project.join("file with spaces.rs");
        fs::write(&file, "hello").unwrap();
        fs::write(home.join("note.md"), "note").unwrap();
        let expected = TerminalLinkFile {
            path: fs::canonicalize(&file).unwrap().to_str().unwrap().into(),
            root: fs::canonicalize(&project).unwrap().to_str().unwrap().into(),
            rel: "file with spaces.rs".into(),
        };
        assert_eq!(
            resolve_file("./file with spaces.rs", &project).unwrap(),
            expected
        );
        assert_eq!(
            resolve_file("../file with spaces.rs", &project.join("nested")).unwrap(),
            expected
        );
        assert_eq!(
            resolve_file(file.to_str().unwrap(), &home).unwrap(),
            expected
        );
        assert_eq!(
            resolve_with_home("~/note.md", &project, Some(&home))
                .unwrap()
                .rel,
            "note.md"
        );
    }

    #[test]
    fn rejects_missing_files_directories_and_control_characters() {
        let fixture = Fixture::new();
        for path in [
            "",
            "missing.rs",
            "project",
            "file\n.rs",
            "file\r.rs",
            "file\0.rs",
            "file\u{1b}.rs",
            "~another/file",
        ] {
            assert!(resolve_file(path, &fixture.0).is_err(), "accepted {path:?}");
        }
        assert!(resolve_with_home("~/note.md", &fixture.0, None).is_err());
        assert!(resolve_file("note.md", Path::new("relative-cwd")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_symlinks_and_treats_shell_metacharacters_literally() {
        let fixture = Fixture::new();
        let name = "$(touch should-not-exist); 'quoted'.rs";
        let file = fixture.0.join("home").join(name);
        fs::write(&file, "hello").unwrap();
        std::os::unix::fs::symlink(&file, fixture.0.join("project/linked.rs")).unwrap();
        let resolved = resolve_file("linked.rs", &fixture.0.join("project")).unwrap();
        assert_eq!(
            resolved.path,
            fs::canonicalize(&file).unwrap().to_str().unwrap()
        );
        assert_eq!(resolved.rel, name);
        assert_eq!(
            resolve_file(name, &fixture.0.join("home")).unwrap(),
            resolved
        );
        assert!(!fixture.0.join("should-not-exist").exists());
    }
}
