//! Distro-specific loopback forwarding. Windows and WSL can both own port 8765;
//! Windows localhost forwarding can then reach the Windows host instead of WSL.
//! Each pooled HTTP connection / SSE stream gets a raw `wsl.exe` pipe into the
//! selected distro. Only bash and cat are required, including for older workers.
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::conn::{LaunchError, SessionHandle};

// Bound per-connection processes/threads, including callers outside the proxy.
const MAX_CONNECTIONS: usize = 128;

pub(super) struct Forward {
    stopped: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    keepalive: Box<dyn SessionHandle>,
}

impl Forward {
    pub(super) fn start(
        distro: String,
        local_port: u16,
        remote_port: u16,
        mut keepalive: Box<dyn SessionHandle>,
    ) -> Result<Self, LaunchError> {
        let listener = match TcpListener::bind(("127.0.0.1", local_port)) {
            Ok(listener) => listener,
            Err(error) => {
                keepalive.teardown();
                let message = format!("Could not bind the WSL forward: {error}");
                return Err(if error.kind() == io::ErrorKind::AddrInUse {
                    LaunchError::PortConflict(message)
                } else {
                    LaunchError::Fatal(message)
                });
            }
        };
        let script = relay_script(remote_port);
        Self::with_listener(listener, keepalive, move || {
            super::ssh::wsl_stream_command(&distro, &script)
        })
    }

    fn with_listener(
        listener: TcpListener,
        mut keepalive: Box<dyn SessionHandle>,
        command: impl Fn() -> Command + Send + 'static,
    ) -> Result<Self, LaunchError> {
        if let Err(error) = listener.set_nonblocking(true) {
            keepalive.teardown();
            return Err(LaunchError::Fatal(format!(
                "Could not configure the WSL forward: {error}"
            )));
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let listener = thread::spawn(move || {
            let mut connections: Vec<(TcpStream, JoinHandle<()>)> = Vec::new();
            while !stop.load(Ordering::Acquire) {
                connections.retain(|(_, worker)| !worker.is_finished());
                match listener.accept() {
                    Ok((socket, _)) => {
                        if connections.len() >= MAX_CONNECTIONS {
                            continue;
                        }
                        let _ = socket.set_nodelay(true);
                        let Ok(control) = socket.try_clone() else {
                            continue;
                        };
                        let command = command();
                        let cancelled = stop.clone();
                        let worker = thread::spawn(move || relay(socket, command, cancelled));
                        connections.push((control, worker));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(10));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        log::debug!("WSL forward listener closed: {error}");
                        break;
                    }
                }
            }
            stop.store(true, Ordering::Release);
            drop(listener);
            // Wake every upload, closing its stdin so bash reaps both cats.
            // Workers also bound the wait and reap their wsl.exe child.
            for (socket, _) in &connections {
                let _ = socket.shutdown(Shutdown::Both);
            }
            for (_, worker) in connections {
                let _ = worker.join();
            }
        });
        Ok(Self {
            stopped,
            listener: Some(listener),
            keepalive,
        })
    }
}

impl SessionHandle for Forward {
    fn is_alive(&self) -> bool {
        !self.stopped.load(Ordering::Acquire)
            && self
                .listener
                .as_ref()
                .is_some_and(|listener| !listener.is_finished())
            && self.keepalive.is_alive()
    }

    fn teardown(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            listener.thread().unpark();
            let _ = listener.join();
            self.keepalive.teardown();
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn relay(socket: TcpStream, mut command: Command, stopped: Arc<AtomicBool>) {
    let Ok(mut upload) = socket.try_clone() else {
        return;
    };
    let Ok(mut download) = socket.try_clone() else {
        return;
    };
    let mut child = match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log::debug!("Could not open WSL stream: {error}");
            return;
        }
    };
    let mut input = child.stdin.take().expect("piped stdin");
    let mut output = child.stdout.take().expect("piped stdout");
    let (done, completed) = mpsc::channel();
    let uploaded = done.clone();
    let upload = thread::spawn(move || {
        let _ = copy_stream(&mut upload, &mut input);
        drop(input);
        let _ = uploaded.send(());
    });
    let download = thread::spawn(move || {
        let _ = copy_stream(&mut output, &mut download);
        let _ = done.send(());
    });
    // HTTP keeps its request side open while reading responses (including SSE).
    // Either EOF means the connection is finished; wake the other direction.
    while !stopped.load(Ordering::Acquire) {
        match completed.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            _ => break,
        }
    }
    let _ = socket.shutdown(Shutdown::Both);
    // Give stdin EOF a chance to clean up inside WSL before killing the launcher.
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let _ = upload.join();
    let _ = download.join();
}

fn copy_stream(reader: &mut impl io::Read, writer: &mut impl io::Write) -> io::Result<()> {
    // Copy each available chunk promptly. std::io::copy's Linux pipe/socket
    // splice path can stall with cat's own splice, including small health/SSE reads.
    let mut bytes = [0; 8192];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => writer.write_all(&bytes[..n])?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn relay_script(remote_port: u16) -> String {
    // Explicit fd 4 keeps background cat off bash's implicit /dev/null stdin.
    // `wait -n` (bash 4.3+) cleans up on EOF from either the client or the host.
    // No READY lines: stdout carries only host-service response bytes.
    format!(
        r#"set -e
exec 3<>/dev/tcp/127.0.0.1/{remote_port}
exec 4<&0
pids=
cleanup() {{
  trap '' HUP INT TERM
  for pid in $pids; do kill "$pid" 2>/dev/null || :; done
  wait 2>/dev/null || :
}}
trap cleanup EXIT
trap 'exit 0' HUP INT TERM
cat <&3 & pids=$!
cat <&4 >&3 & pids="$pids $!"
wait -n
"#
    )
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn test_forward(local_port: u16, remote_port: u16) -> Box<dyn SessionHandle> {
    struct Keepalive;
    impl SessionHandle for Keepalive {
        fn is_alive(&self) -> bool {
            true
        }
        fn teardown(&mut self) {}
    }
    let listener = TcpListener::bind(("127.0.0.1", local_port)).unwrap();
    let script = relay_script(remote_port);
    Box::new(
        Forward::with_listener(listener, Box::new(Keepalive), move || {
            let mut command = skill_core::process::hidden_command("bash");
            command
                .env_remove("BASH_ENV")
                .args(["--noprofile", "--norc", "-c", &script]);
            command
        })
        .unwrap_or_else(|_| panic!("test forward failed")),
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn connection(port: u16) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn a_stalled_upload_cannot_block_disconnect() {
        struct Keepalive;
        impl SessionHandle for Keepalive {
            fn is_alive(&self) -> bool {
                true
            }
            fn teardown(&mut self) {}
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut forward = Forward::with_listener(listener, Box::new(Keepalive), || {
            let mut command = skill_core::process::hidden_command("bash");
            command.env_remove("BASH_ENV").args([
                "--noprofile",
                "--norc",
                "-c",
                "echo READY; exec sleep 30",
            ]);
            command
        })
        .unwrap_or_else(|_| panic!("test forward failed"));
        let mut socket = connection(port);
        let mut ready = [0; 6];
        socket.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"READY\n");
        let mut writer = socket.try_clone().unwrap();
        let upload = thread::spawn(move || writer.write_all(&vec![0; 16 * 1024 * 1024]));
        thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        forward.teardown();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "blocked pipe must be cancelled"
        );
        assert!(upload.join().unwrap().is_err());
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn forwards_concurrent_binary_streams_and_releases_connections_on_teardown() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote_port = server.local_addr().unwrap().port();
        let local_port = free_port();
        let mut forward = test_forward(local_port, remote_port);
        let server = thread::spawn(move || {
            let mut clients = Vec::new();
            for _ in 0..4 {
                let (mut socket, _) = server.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                clients.push(thread::spawn(move || {
                    let mut bytes = [0; 8192];
                    loop {
                        match socket.read(&mut bytes) {
                            Ok(0) => return,
                            Ok(n) => socket.write_all(&bytes[..n]).unwrap(),
                            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return,
                            Err(error) => panic!("remote stream did not close: {error}"),
                        }
                    }
                }));
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        let clients: Vec<_> = (0..4)
            .map(|_| {
                thread::spawn(move || {
                    let mut socket = connection(local_port);
                    let payload: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
                    let mut writer = socket.try_clone().unwrap();
                    let sending = payload.clone();
                    let upload = thread::spawn(move || writer.write_all(&sending).unwrap());
                    let mut received = vec![0; payload.len()];
                    socket.read_exact(&mut received).unwrap();
                    assert_eq!(received, payload);
                    upload.join().unwrap();
                    socket
                })
            })
            .collect();
        let mut sockets: Vec<_> = clients
            .into_iter()
            .map(|client| client.join().unwrap())
            .collect();
        assert!(forward.is_alive());
        let start = Instant::now();
        forward.teardown();
        forward.teardown(); // idempotent, including Drop
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(!forward.is_alive());
        for socket in &mut sockets {
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        }
        let _rebound = TcpListener::bind(("127.0.0.1", local_port)).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn idle_sse_and_client_disconnect_leave_the_remote_listener_running() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let remote_port = server.local_addr().unwrap().port();
        let local_port = free_port();
        let forward = test_forward(local_port, remote_port);
        let server = thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket.write_all(b"data: first\n\n").unwrap();
            thread::sleep(Duration::from_millis(150));
            socket.write_all(b"data: second\n\n").unwrap();
            assert_eq!(
                socket.read(&mut [0]).unwrap(),
                0,
                "client EOF must reach WSL"
            );
            // A disconnected accessor must not stop the durable host.
            let (mut next, _) = server.accept().unwrap();
            next.write_all(b"still alive").unwrap();
        });
        let mut socket = connection(local_port);
        let mut events = [0; 27];
        socket.read_exact(&mut events).unwrap();
        assert_eq!(&events, b"data: first\n\ndata: second\n\n");
        drop(socket);
        let mut next = connection(local_port);
        let mut response = String::new();
        next.read_to_string(&mut response).unwrap();
        assert_eq!(response, "still alive");
        server.join().unwrap();
        drop(forward);
        let _rebound = TcpListener::bind(("127.0.0.1", local_port)).unwrap();
    }
}
