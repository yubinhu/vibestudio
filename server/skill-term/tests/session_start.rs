//! Exercise real tmux session creation in an isolated process. A desktop host
//! can outlive its launch directory; login profiles may read project-relative
//! settings and then change cwd. Both initialization and the agent must use the
//! selected project.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use skill_core::process::hidden_command;

const CHILD: &str = "VIBESTUDIO_TEST_SESSION_START";
const SURVIVOR: &str = "fixture-survivor";

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

struct Fixture {
    root: PathBuf,
    tmux_bin: PathBuf,
}

impl Fixture {
    fn new(tmux_bin: PathBuf) -> Self {
        // Keep the Unix socket below macOS's short sockaddr_un path limit.
        let root = PathBuf::from("/tmp").join(format!(
            "vs-cwd-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        for name in [
            "home",
            "config",
            "tmux",
            "tmp",
            "bin",
            "deleted",
            "elsewhere",
            "project ' with spaces",
        ] {
            fs::create_dir(root.join(name)).unwrap();
        }
        symlink(&tmux_bin, root.join("bin/tmux")).unwrap();
        symlink(
            root.join("project ' with spaces"),
            root.join("selected-link"),
        )
        .unwrap();
        let fixture = Self { root, tmux_bin };
        fs::write(
            fixture.root.join("project ' with spaces/.project-env"),
            "export VIBESTUDIO_TEST_PROJECT_ENV=selected-project\n",
        )
        .unwrap();
        // Restrict discovery to system utilities and our tmux: no real coding
        // agents, user startup scripts, tmux settings, or secrets are involved.
        fs::write(
            fixture.root.join("home/.bash_profile"),
            format!(
                "export PATH={}\nexport VIBESTUDIO_TEST_LOGIN_PROFILE=loaded\nif [ -f ./.project-env ]; then . ./.project-env; fi\nprintf 'profile ran\\n' >> {}\nbuiltin cd -- {}\nset -o noclobber\n",
                quote(Path::new(&fixture.path())),
                quote(&fixture.root.join("login-startups")),
                quote(&fixture.root.join("elsewhere")),
            ),
        ).unwrap();
        fixture
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.root.join("bin").display())
    }

    fn command(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = hidden_command(executable);
        command
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("TMUX_TMPDIR", self.root.join("tmux"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("PATH", self.path())
            .env("SHELL", "/bin/bash")
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn tmux(&self, args: &[&str]) -> Output {
        self.command(&self.tmux_bin).args(args).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // This server's socket lives only in this fixture's private directory.
        let _ = self.tmux(&["kill-server"]);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_child(root: &Path) {
    fs::remove_dir(root.join("deleted")).unwrap();
    assert!(
        std::env::current_dir().is_err(),
        "fixture must reproduce a stale inherited cwd"
    );

    let selected = root.join("selected-link");
    let session = skill_term::create_session_cmd(
        "shell",
        selected.to_str().unwrap(),
        80,
        24,
        "test \"$VIBESTUDIO_TEST_LOGIN_PROFILE\" = loaded && test \"$VIBESTUDIO_TEST_PROJECT_ENV\" = selected-project && /bin/pwd -P > session-pwd",
    )
    .expect("new session must recover from the deleted host cwd");
    assert_eq!(
        session.cwd,
        selected.to_string_lossy(),
        "preserve selected path spelling for resume"
    );
    let marker = selected.join("session-pwd");
    let expected = fs::canonicalize(&selected).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(&marker).is_ok_and(|text| text.trim() == expected.to_string_lossy()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the real pane did not initialize and run in the selected folder"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        root.join("login-startups").exists(),
        "fixture login profile must actually run"
    );
    assert!(
        !root.join("elsewhere/session-pwd").exists(),
        "profile cwd must not override the selection"
    );
    skill_term::kill_session(&session.id).unwrap();
    assert!(
        skill_term::list_sessions().unwrap().is_empty(),
        "closed session must leave no app session behind"
    );

    let missing = root.join("missing-project");
    let error = match skill_term::create_session_cmd(
        "shell",
        missing.to_str().unwrap(),
        80,
        24,
        "/bin/pwd -P > must-not-start",
    ) {
        Err(error) => error,
        Ok(session) => panic!("missing folder unexpectedly created {}", session.id),
    };
    assert!(
        error.contains(missing.to_str().unwrap()),
        "error must identify the selected folder: {error}"
    );
    assert!(
        skill_term::list_sessions().unwrap().is_empty(),
        "rejected create must not leave a stub session"
    );
}

#[test]
fn selected_folder_survives_deleted_launcher_cwd_and_login_cd() {
    if let Some(root) = std::env::var_os(CHILD) {
        run_child(Path::new(&root));
        return;
    }
    let found = hidden_command("/bin/sh")
        .args(["-c", "command -v tmux"])
        .current_dir("/")
        .output()
        .unwrap();
    if !found.status.success() {
        eprintln!("skipping real session startup test: tmux is not installed");
        return;
    }
    let tmux = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim());
    let fixture = Fixture::new(tmux);
    let existing = fixture.tmux(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        SURVIVOR,
        "-c",
        "/",
        "/bin/sleep",
        "60",
    ]);
    assert!(
        existing.status.success(),
        "{}",
        String::from_utf8_lossy(&existing.stderr)
    );

    let mut child = fixture
        .command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "selected_folder_survives_deleted_launcher_cwd_and_login_cd",
            "--nocapture",
        ])
        .env(CHILD, &fixture.root)
        .current_dir(fixture.root.join("deleted"))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = fixture.tmux(&["kill-server"]);
            let _ = child.kill();
            let _ = child.wait();
            panic!("session startup subprocess did not finish within 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child stdout:\n{}\nchild stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let survivors = fixture.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert!(
        survivors.status.success(),
        "fixture's pre-existing session must remain alive"
    );
    assert_eq!(
        String::from_utf8(survivors.stdout).unwrap().trim(),
        SURVIVOR,
        "creating, closing, and rejecting new sessions must leave the existing session untouched"
    );
}
