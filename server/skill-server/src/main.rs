// CLI entry for the standalone `skill-server` — the unit that runs on a remote host
// reached over an `ssh -L` tunnel (provisioned by the desktop's connection manager),
// or for browser-local dev (`cargo run -p skill-server`). The serve loop lives in the
// library (src/lib.rs); this only parses argv, prints a machine-readable ready line,
// or starts/attaches the detached per-user host service. Desktop uses the same
// worker API through a headless flag on its own executable.
//
// Usage: skill-server [--daemon] [--host H] [--port N] [--dist PATH] [--token T]
//   --daemon        ensure the durable per-user worker, print its record, then exit
//   --host-service  run the detached worker (internal; shared with desktop)
//   --stop-host-service  explicitly stop the worker; tmux agents keep running
//   --port 0        bind an ephemeral port (the chosen port is printed in the ready line)
//   --token T       require `Authorization: Bearer T` on every request (the SSH case).
//                   Prefer the VIBESTUDIO_SERVER_TOKEN env var (keeps the token off
//                   the process command line); `--token` overrides it for manual use.
//   --lifeline-stdin  legacy compatibility only: exit when stdin hits EOF — old clients hold the channel's
//                     stdin open, so the server dies the instant that session drops
//                     (orphan prevention; pairs with ssh ServerAlive + the held pipe)
//   --mobile-dev    serve the PHONE experience in a desktop browser: wire a
//                   file-backed SecureStore (config dir) so /api/remote/profiles*
//                   + the connect flow light up and the SPA renders mobile mode.
//                   No Mac/simulator needed. Pair with `--features russh-transport`
//                   for the in-process russh transport + on-device keygen (the
//                   exact iOS path); without it, connect falls back to system `ssh`.
//   defaults: --host 127.0.0.1  --port 8765  --dist ./dist  (override with SKILL_DIST)
//
// On start it prints exactly one machine-readable line to stdout, flushed first:
//   SKILL_SERVER_READY port=<port>
// The desktop line-scans for this to learn the ephemeral port.
use std::io::{Read, Write};
use std::path::PathBuf;

use skill_server::{
    init_logging, spawn, PhoneControl, RemoteControl, ServerConfig, SshRemoteControl,
};
use std::sync::Arc;

fn main() {
    // Install the logger first (stderr; level via RUST_LOG) so even early failures
    // are captured. Keep it off stdout — that's the SKILL_SERVER_READY channel.
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        #[cfg(feature = "local-backend")]
        println!("skill-server {} host-service=1", env!("CARGO_PKG_VERSION"));
        #[cfg(not(feature = "local-backend"))]
        println!("skill-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    #[cfg(feature = "local-backend")]
    if let Some(result) = skill_server::host_service::run_from_args(&args) {
        if let Err(error) = result {
            log::error!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8765;
    let mut dist = std::env::var("SKILL_DIST").unwrap_or_else(|_| "dist".to_string());
    // Prefer the token from the env (VIBESTUDIO_SERVER_TOKEN) — the desktop delivers
    // it that way so it stays off the world-readable command line; `--token` still
    // works for manual/standalone use and overrides the env.
    let mut token: Option<String> = std::env::var("VIBESTUDIO_SERVER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let mut lifeline_stdin = false;
    let mut mobile_dev = false;
    let mut daemon = false;
    #[cfg(feature = "local-backend")]
    let mut recovery = false;
    let mut startup_maintenance = true;
    let mut bundled_skills: Option<PathBuf> = None;
    let mut examples_base: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = args.get(i).cloned().unwrap_or(host);
            }
            "--port" => {
                i += 1;
                port = args.get(i).and_then(|p| p.parse().ok()).unwrap_or(port);
            }
            "--dist" => {
                i += 1;
                dist = args.get(i).cloned().unwrap_or(dist);
            }
            "--token" => {
                i += 1;
                token = args.get(i).cloned().filter(|t| !t.is_empty());
            }
            "--lifeline-stdin" => lifeline_stdin = true,
            "--mobile-dev" => mobile_dev = true,
            "--daemon" => daemon = true,
            #[cfg(feature = "local-backend")]
            "--recover" => recovery = true,
            "--no-startup-maintenance" => startup_maintenance = false,
            "--bundled-skills" => {
                i += 1;
                bundled_skills = args.get(i).map(PathBuf::from);
            }
            "--examples-base" => {
                i += 1;
                examples_base = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }
    let dist = PathBuf::from(dist);
    if daemon {
        #[cfg(feature = "local-backend")]
        {
            if !matches!(host.as_str(), "127.0.0.1" | "localhost")
                || token.is_some()
                || lifeline_stdin
                || mobile_dev
            {
                log::error!("--daemon runs a tokenless loopback host service; it cannot be combined with a non-loopback host, token, lifeline or mobile switchboard.");
                std::process::exit(1);
            }
            let options = skill_server::host_service::HostServiceOptions {
                dist,
                bundled_skills,
                examples_base,
                preferred_port: port,
                startup_maintenance,
                recovery,
                ..Default::default()
            };
            match skill_server::host_service::ensure(&options) {
                Ok(record) => {
                    println!(
                        "SKILL_HOST_SERVICE_READY {}",
                        serde_json::to_string(&record).expect("host service record")
                    );
                    println!("SKILL_SERVER_READY port={}", record.port);
                }
                Err(error) => {
                    log::error!("{error}");
                    std::process::exit(1);
                }
            }
            return;
        }
        #[cfg(not(feature = "local-backend"))]
        {
            log::error!("This build has no local backend and cannot run a host service.");
            std::process::exit(1);
        }
    }
    let bind = format!("{host}:{port}");

    // Expose the SSH connection manager (so `/api/remote/*` works and the UI shows the
    // connect pill) only for an interactive, loopback server the user controls — NOT a
    // provisioned remote (`--lifeline-stdin`, to avoid surprise nested onward-ssh) and
    // NOT a server bound to a public interface (so a bare public server can't be used
    // to ssh outward). The desktop wires the SAME manager into its in-process server.
    let is_loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    // `--mobile-dev`: a file-backed SecureStore turns this standalone server into
    // the phone switchboard for browser development. The store is wired into BOTH
    // the connection manager (so `connect(id)` resolves a saved profile) and the
    // server config (so the profile routes are served) — exactly as the iOS shell
    // wires its Keychain store.
    let secure_store: Option<Arc<dyn skill_server::SecureStore>> = if mobile_dev {
        match skill_core::paths::ensure_config_dir()
            .and_then(|dir| skill_core::keystore::resolve(None, &dir))
        {
            Ok(s) => Some(s),
            Err(e) => {
                log::error!("--mobile-dev: couldn't open the credential store: {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let remote: Option<std::sync::Arc<dyn RemoteControl>> = if !lifeline_stdin && is_loopback {
        Some(std::sync::Arc::new(SshRemoteControl::with_secure_store(
            env!("CARGO_PKG_VERSION").to_string(),
            secure_store.clone(),
        )))
    } else {
        None
    };
    // Every loopback server offers "Open on your phone" — enable() fronts THIS
    // server with the tailscale of the machine it runs on. That includes a
    // provisioned remote: the stable server is the hub the phone reaches
    // directly, so a sleeping client machine costs nothing. (The desktop client
    // itself still connects over SSH, never the tailnet.)
    let phone = if is_loopback {
        Some(std::sync::Arc::new(PhoneControl::new(
            env!("CARGO_PKG_VERSION").to_string(),
        )))
    } else {
        None
    };

    let cfg = ServerConfig {
        host,
        port,
        dist: dist.clone(),
        token: token.clone(),
        startup_maintenance,
        bundled_skills,
        examples_base,
        remote,
        phone: phone.clone(),
        secure_store,
        ..Default::default()
    };
    let handle = match spawn(cfg) {
        Ok(h) => h,
        Err(e) => {
            log::error!("failed to bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    if let Some(p) = &phone {
        p.set_port(handle.addr.port());
        // Re-point a persisted `tailscale serve` mapping if this boot's port drifted.
        // Test/development servers with maintenance disabled must not replace
        // the real host service's machine-wide Tailscale mapping.
        if startup_maintenance {
            p.clone().resync_on_start();
        }
    }

    // Machine-readable ready line FIRST (flushed), so the desktop can read back the
    // ephemeral port even when other logging follows. The token is NOT echoed here —
    // the client already holds it, and stdout could be logged.
    {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "SKILL_SERVER_READY port={}", handle.addr.port());
        let _ = out.flush();
    }
    println!(
        "skill-server listening on {}  (dist: {})",
        handle.url(),
        dist.display()
    );
    if !dist.join("index.html").is_file() {
        // Remote installs serve no UI (the desktop's local server does), so this is
        // expected there; it only matters for a standalone browser-local run.
        println!(
            "  note: {} has no index.html — the UI is served by the client.",
            dist.display()
        );
    }

    // Tie our lifetime to the SSH session: the desktop launches us as `ssh … 'exec
    // skill-server --lifeline-stdin'` while holding that channel's stdin open. When
    // the session drops (disconnect, desktop exit, or a hard kill closing the pipe),
    // stdin hits EOF and we exit — no orphaned server lingers on the remote.
    if lifeline_stdin {
        std::thread::spawn(|| {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 256];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => std::process::exit(0), // EOF or broken pipe
                    Ok(_) => {}                              // ignore any input bytes
                }
            }
        });
    }

    handle.join();
}
