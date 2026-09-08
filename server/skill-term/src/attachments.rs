//! Viewer attachments have their own identity, PTY, and geometry. tmux remains
//! the durable session; closing a viewer only kills its `attach-session` child.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use super::{new_uuid, size_floor, valid_session_name, MIN_COLS, MIN_ROWS};

const GEOMETRY_OWNER: &str = "@ass_geometry_owner";

#[derive(Clone, Default)]
struct Tmux {
    // A private socket lets integration tests exercise real tmux/PTY behavior
    // without changing any live session or the user's tmux configuration.
    socket: Option<PathBuf>,
}

impl Tmux {
    fn command(&self) -> Command {
        let mut command = super::tmux();
        if let Some(socket) = &self.socket {
            command.arg("-S").arg(socket);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        let output = self
            .command()
            .args(args)
            .output()
            .map_err(super::tmux_spawn_err)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "tmux: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn geometry(
        &self,
        session: &str,
        token: &str,
        cols: u16,
        rows: u16,
        mode: Geometry,
    ) -> Result<(), String> {
        let target = format!("={session}:");
        let window = format!("={session}:");
        // All interpolated values are internally minted identifiers or numeric
        // dimensions. This is tmux command syntax, never a shell command.
        let resize = format!("resize-window -t {window} -x {cols} -y {rows}");
        let claim = format!("set-option -t {target} {GEOMETRY_OWNER} {token}; {resize}");
        let owned = format!("#{{==:#{{{GEOMETRY_OWNER}}},{token}}}");
        match mode {
            Geometry::Claim => self.run(&[
                "set-option",
                "-t",
                &target,
                GEOMETRY_OWNER,
                token,
                ";",
                "resize-window",
                "-t",
                &window,
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ]),
            Geometry::Passive => self.run(&["if-shell", "-F", "-t", &window, &owned, &resize]),
            Geometry::Initial => {
                // The first viewer may set the size. A later passive viewer
                // cannot steal it, including when it uses a sibling backend.
                // Zero live clients also recovers an owner left by a crash.
                let vacant = format!(
                    "#{{||:#{{==:#{{{GEOMETRY_OWNER}}},}},#{{==:#{{session_attached}},0}}}}"
                );
                self.run(&["if-shell", "-F", "-t", &window, &vacant, &claim])
            }
            Geometry::Release => {
                let release = format!("set-option -u -t {target} {GEOMETRY_OWNER}");
                self.run(&["if-shell", "-F", "-t", &window, &owned, &release])
            }
        }
    }
}

enum Geometry {
    Initial,
    Claim,
    Passive,
    Release,
}

struct Io {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    cols: u16,
    rows: u16,
}

/// A live viewer of a durable tmux session. Its opaque ID never identifies a
/// different viewer after detach/reconnect, even when the session is the same.
pub struct Attachment {
    session: String,
    token: String,
    tmux: Tmux,
    closed: AtomicBool,
    io: Mutex<Io>,
}

impl Attachment {
    pub fn attachment_id(&self) -> &str {
        &self.token
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn write_bytes(&self, data: &[u8], claim_geometry: bool) -> Result<(), String> {
        let mut io = self.io.lock().map_err(|_| "Terminal is unavailable.")?;
        self.require_open()?;
        if !data.is_empty() && claim_geometry {
            self.tmux.geometry(
                &self.session,
                &self.token,
                io.cols,
                io.rows,
                Geometry::Claim,
            )?;
        }
        io.writer
            .write_all(data)
            .and_then(|_| io.writer.flush())
            .map_err(|e| e.to_string())
    }

    fn resize_to(&self, cols: u16, rows: u16, claim_geometry: bool) -> Result<(), String> {
        if cols < MIN_COLS || rows < MIN_ROWS {
            return Err(format!("implausible terminal size {cols}x{rows} — refused"));
        }
        let mut io = self.io.lock().map_err(|_| "Terminal is unavailable.")?;
        self.require_open()?;
        io.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        io.cols = cols;
        io.rows = rows;
        self.tmux.geometry(
            &self.session,
            &self.token,
            cols,
            rows,
            if claim_geometry {
                Geometry::Claim
            } else {
                Geometry::Passive
            },
        )
    }

    fn require_open(&self) -> Result<(), String> {
        if self.is_closed() {
            Err("That terminal attachment is no longer active.".into())
        } else {
            Ok(())
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut registry) = registry().lock() {
            registry.remove(&self.token);
        }
        // Serialize close with input/resize. Killing this one child closes its
        // reader channel and wakes its SSE owner without touching other viewers.
        if let Ok(mut io) = self.io.lock() {
            let _ = io.child.kill();
            let _ = io.child.wait();
            let _ = self.tmux.geometry(
                &self.session,
                &self.token,
                io.cols,
                io.rows,
                Geometry::Release,
            );
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.close();
    }
}

struct Registration {
    session: String,
    attachment: Weak<Attachment>,
}
type Registry = Mutex<HashMap<String, Registration>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Attach a new viewer. The stream owner must retain the Arc and send its
/// `attachment_id` before forwarding any bytes received from the channel.
pub fn attach(
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(Arc<Attachment>, Receiver<Vec<u8>>), String> {
    attach_with(id, cols, rows, Tmux::default())
}

fn attach_with(
    id: &str,
    cols: u16,
    rows: u16,
    tmux: Tmux,
) -> Result<(Arc<Attachment>, Receiver<Vec<u8>>), String> {
    if !valid_session_name(id) {
        return Err("That terminal session no longer exists.".into());
    }
    let target = format!("={id}");
    tmux.run(&["has-session", "-t", &target])?;
    let (cols, rows) = size_floor("attach", id, cols, rows);
    // Automatic `latest`/`smallest` sizing couples every viewer to all others.
    // In manual mode each attach PTY has its own size; only explicit owner
    // commands below resize the shared agent window.
    tmux.run(&[
        "set-option",
        "-w",
        "-t",
        &format!("={id}:"),
        "window-size",
        "manual",
    ])?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;
    let mut command = CommandBuilder::new(super::tmux_bin());
    command.arg("-u");
    if let Some(socket) = &tmux.socket {
        command.arg("-S");
        command.arg(socket);
    }
    command.args(["attach-session", "-t", &target]);
    command.env("TERM", "xterm-256color");
    command.env_remove("TMUX");
    command.env_remove("TMUX_PANE");
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    if let Some(home) = dirs::home_dir() {
        command.env("HOME", home.to_string_lossy().into_owned());
    }
    if let Ok(tmp) = std::env::var("TMUX_TMPDIR") {
        command.env("TMUX_TMPDIR", tmp);
    }
    let token = new_uuid();
    // Check whether any viewer exists before our own child registers. Otherwise
    // a stale owner left by a crash can look live solely because we just joined.
    tmux.geometry(id, &token, cols, rows, Geometry::Initial)?;
    let mut child = match pair.slave.spawn_command(command) {
        Ok(child) => child,
        Err(error) => {
            let _ = tmux.geometry(id, &token, cols, rows, Geometry::Release);
            return Err(format!("Couldn't attach: {error}"));
        }
    };
    drop(pair.slave);
    let wire = pair
        .master
        .try_clone_reader()
        .and_then(|reader| pair.master.take_writer().map(|writer| (reader, writer)));
    let (mut reader, writer) = match wire {
        Ok(wire) => wire,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = tmux.geometry(id, &token, cols, rows, Geometry::Release);
            return Err(error.to_string());
        }
    };
    let attachment = Arc::new(Attachment {
        session: id.to_string(),
        token,
        tmux,
        closed: AtomicBool::new(false),
        io: Mutex::new(Io {
            master: pair.master,
            child,
            writer,
            cols,
            rows,
        }),
    });
    registry()
        .lock()
        .map_err(|_| "Terminal registry is unavailable.")?
        .insert(
            attachment.token.clone(),
            Registration {
                session: id.to_string(),
                attachment: Arc::downgrade(&attachment),
            },
        );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) if tx.send(buffer[..n].to_vec()).is_err() => break,
                Ok(_) => {}
            }
        }
    });
    Ok((attachment, rx))
}

fn resolve(id: &str, attachment_id: Option<&str>) -> Result<Arc<Attachment>, String> {
    // Clone Weak references while locked; upgrade/drop Arcs after unlocking so
    // Attachment::drop can remove its own registration without deadlocking.
    let candidates: Vec<Weak<Attachment>> = {
        let registry = registry()
            .lock()
            .map_err(|_| "Terminal registry is unavailable.")?;
        if let Some(token) = attachment_id {
            registry
                .get(token)
                .filter(|entry| entry.session == id)
                .map(|entry| vec![entry.attachment.clone()])
                .unwrap_or_default()
        } else {
            registry
                .values()
                .filter(|entry| entry.session == id)
                .map(|entry| entry.attachment.clone())
                .collect()
        }
    };
    let mut live = candidates
        .into_iter()
        .filter_map(|weak| weak.upgrade())
        .filter(|attachment| !attachment.is_closed());
    let first = live
        .next()
        .ok_or("That terminal attachment is no longer active.")?;
    if live.next().is_some() {
        return Err("Multiple viewers are attached; an attachmentId is required.".into());
    }
    Ok(first)
}

pub fn write_attachment(
    id: &str,
    attachment_id: Option<&str>,
    data: &[u8],
    claim_geometry: bool,
) -> Result<(), String> {
    resolve(id, attachment_id)?.write_bytes(data, claim_geometry)
}

pub fn resize_attachment(
    id: &str,
    attachment_id: Option<&str>,
    cols: u16,
    rows: u16,
    claim_geometry: bool,
) -> Result<(), String> {
    resolve(id, attachment_id)?.resize_to(cols, rows, claim_geometry)
}

/// Legacy session-only input is safe only while there is exactly one viewer.
pub fn write(id: &str, data: &[u8]) -> Result<(), String> {
    write_attachment(id, None, data, true)
}
pub fn resize(id: &str, cols: u16, rows: u16) -> Result<(), String> {
    resize_attachment(id, None, cols, rows, false)
}

/// Explicitly close exactly this viewer. The stable tmux session remains alive.
pub fn detach(id: &str, attachment_id: &str) -> Result<(), String> {
    resolve(id, Some(attachment_id))?.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Session {
        id: String,
        tmux: Tmux,
    }

    impl Session {
        fn create() -> Option<Self> {
            if super::super::tmux().arg("-V").output().is_err() {
                eprintln!("tmux unavailable — skipping viewer integration test");
                return None;
            }
            let id = format!(
                "ass-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                super::super::SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let tmux = Tmux {
                socket: Some(PathBuf::from(format!("/tmp/vs-view-{}.sock", new_uuid()))),
            };
            // Arm cleanup before creating the server so setup failures also
            // cannot leave a private test shell behind.
            let session = Self { id, tmux };
            session
                .tmux
                .run(&[
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    &session.id,
                    "-x",
                    "100",
                    "-y",
                    "30",
                    "bash",
                    "--noprofile",
                    "--norc",
                    "-i",
                ])
                .unwrap();
            session
                .tmux
                .run(&["set-option", "-t", &session.id, "status", "off"])
                .unwrap();
            Some(session)
        }

        fn attach(&self, cols: u16, rows: u16) -> (Arc<Attachment>, Receiver<Vec<u8>>) {
            attach_with(&self.id, cols, rows, self.tmux.clone()).unwrap()
        }

        fn value(&self, format: &str) -> String {
            let output = self
                .tmux
                .command()
                .args(["display-message", "-p", "-t", &self.id, format])
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        }

        fn size(&self) -> String {
            self.value("#{window_width}x#{window_height}")
        }

        fn wait_clients(&self, expected: usize) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.value("#{session_attached}") != expected.to_string() {
                assert!(
                    Instant::now() < deadline,
                    "expected {expected} live clients"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.tmux.run(&["kill-server"]);
        }
    }

    fn output_contains(rx: &Receiver<Vec<u8>>, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = String::new();
        while Instant::now() < deadline {
            if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(50)) {
                output.push_str(&String::from_utf8_lossy(&bytes));
                if output.contains(needle) {
                    return;
                }
            }
        }
        panic!("missing terminal output {needle:?}: {output:?}");
    }

    #[test]
    fn two_viewers_keep_input_geometry_and_lifetimes_separate() {
        let Some(session) = Session::create() else {
            return;
        };
        let (first, first_output) = session.attach(100, 30);
        session.wait_clients(1);
        let first_id = first.attachment_id().to_string();
        let (second, second_output) = session.attach(50, 12);
        session.wait_clients(2);
        let second_id = second.attachment_id().to_string();
        assert_ne!(first_id, second_id);
        assert_eq!(
            session.size(),
            "100x30",
            "passive attach must not resize the agent"
        );
        assert!(write(&session.id, b"should not be written").is_err());
        assert!(resize(&session.id, 80, 24).is_err());

        resize_attachment(&session.id, Some(&second_id), 60, 14, false).unwrap();
        assert_eq!(
            second.io.lock().unwrap().master.get_size().unwrap().cols,
            60
        );
        assert_eq!(
            session.size(),
            "100x30",
            "passive viewer only resizes its own PTY"
        );
        write_attachment(
            &session.id,
            Some(&second_id),
            b"printf 'viewer-%s-%s\\n' second one\r",
            false,
        )
        .unwrap();
        output_contains(&second_output, "viewer-second-one");
        assert_eq!(
            session.size(),
            "100x30",
            "automatic input does not claim geometry"
        );

        write_attachment(
            &session.id,
            Some(&second_id),
            b"printf 'viewer-%s-%s\\n' second two\r",
            true,
        )
        .unwrap();
        output_contains(&first_output, "viewer-second-two");
        output_contains(&second_output, "viewer-second-two");
        assert_eq!(
            session.size(),
            "60x14",
            "user input claims this viewer's dimensions"
        );
        resize_attachment(&session.id, Some(&first_id), 120, 40, false).unwrap();
        assert_eq!(session.size(), "60x14");
        resize_attachment(&session.id, Some(&second_id), 65, 15, false).unwrap();
        assert_eq!(
            session.size(),
            "65x15",
            "only the current owner can passively resize shared geometry"
        );

        drop(first);
        session.wait_clients(1);
        assert!(write_attachment(&session.id, Some(&first_id), b"stale", true).is_err());
        write(&session.id, b"printf 'viewer-%s-%s\\n' remaining alive\r").unwrap();
        output_contains(&second_output, "viewer-remaining-alive");
        assert!(!second.is_closed());
        assert_eq!(session.size(), "65x15");
    }

    #[test]
    fn explicit_detach_and_reconnect_never_reuse_an_attachment_identity() {
        let Some(session) = Session::create() else {
            return;
        };
        let (old, old_output) = session.attach(100, 30);
        session.wait_clients(1);
        let old_id = old.attachment_id().to_string();
        let (other, other_output) = session.attach(80, 24);
        session.wait_clients(2);
        detach(&session.id, &old_id).unwrap();
        session.wait_clients(1);
        assert!(
            old.is_closed(),
            "detach closes even while the stream retains its Arc"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match old_output.recv_timeout(Duration::from_millis(50)) {
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => assert!(
                    Instant::now() < deadline,
                    "detach must wake the stream owner"
                ),
            }
        }
        let (new, new_output) = session.attach(90, 25);
        session.wait_clients(2);
        assert_ne!(old_id, new.attachment_id());
        assert!(write_attachment(&session.id, Some(&old_id), b"stale", true).is_err());
        assert!(resize_attachment(&session.id, Some(&old_id), 150, 60, true).is_err());
        assert!(detach(&session.id, &old_id).is_err());
        assert!(write_attachment(
            "ass-1-2-3",
            Some(new.attachment_id()),
            b"wrong session",
            true
        )
        .is_err());
        assert!(write(&session.id, b"ambiguous").is_err());
        detach(&session.id, other.attachment_id()).unwrap();
        session.wait_clients(1);
        drop(old); // a late old stream teardown must not remove its replacement
        write_attachment(
            &session.id,
            Some(new.attachment_id()),
            b"printf 'reconnect-%s\\n' alive\r",
            true,
        )
        .unwrap();
        output_contains(&new_output, "reconnect-alive");
        assert!(other.is_closed());
        drop(other_output);
        assert_eq!(session.size(), "90x25");
        assert_eq!(
            session.value("#{session_name}"),
            session.id,
            "the durable session survives viewer detach"
        );
    }

    #[test]
    fn first_viewer_recovers_geometry_owner_left_by_a_crashed_backend() {
        let Some(session) = Session::create() else {
            return;
        };
        session
            .tmux
            .run(&[
                "set-option",
                "-t",
                &session.id,
                GEOMETRY_OWNER,
                "stale-owner",
            ])
            .unwrap();
        assert_eq!(session.value("#{session_attached}"), "0");
        let (viewer, _output) = session.attach(90, 25);
        session.wait_clients(1);
        assert_eq!(session.size(), "90x25");
        assert_eq!(
            session.value("#{@ass_geometry_owner}"),
            viewer.attachment_id()
        );
    }
}
