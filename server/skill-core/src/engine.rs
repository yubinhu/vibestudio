//! On-device LLM engine: a managed `llama-server` (llama.cpp) child process.
//!
//! We shell out to the battle-tested prebuilt `llama-server` rather than linking
//! an inference library into the binary — same philosophy as `gitops` shelling
//! out to `git` and `skill_term` supervising child processes, and it keeps the
//! release profile's small-binary goals intact (no cmake / C++ toolchain).
//!
//! Lifecycle: lazily spawned on first use, bound to `127.0.0.1` on an ephemeral
//! port, kept warm in a global for the session, and reaped on app/server exit.
//! GPU offload is delegated to llama-server's own VRAM-aware fitting (`-ngl auto`
//! with the default `--fit on`): it offloads as many layers as fit the detected
//! device(s) within a per-device margin and runs the rest on CPU, resolving to
//! pure CPU when no GPU backend is loaded — so it runs everywhere. GPU backends
//! (Vulkan/CUDA/…) load dynamically from sibling `libggml-*` libs when present.
//! The model is a single GGUF downloaded on first use (or pointed at a local
//! file for airgapped installs), never bundled in the binary.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::process::hidden_command;

// ───────────────────────────── model catalog ─────────────────────────────

/// A downloadable GGUF model + the context window to run it with.
struct ModelSpec {
    /// Stable id, surfaced in `ModelStatus` for the UI.
    id: &'static str,
    /// Hugging Face repo (`<owner>/<name>`).
    repo: &'static str,
    /// GGUF filename within the repo.
    file: &'static str,
    /// Expected SHA-256 of the file. `None` skips verification.
    sha256: Option<&'static str>,
    /// Context window to start the server with (tokens).
    ctx: u32,
}

/// The on-device model: Qwen3.5-2B, Q4_K_M (~1.4 GB, downloaded on first run). The
/// 0.6B was tried but isn't smart enough for this task — it anchored on the top of
/// the diff (the skill's `description:`) instead of describing the change. Qwen3.5
/// defaults thinking ON under `--jinja` and, for this model, buries the answer in
/// `reasoning_content` leaving `content` empty, so we DISABLE it via
/// `chat_template_kwargs {enable_thinking:false}`. Message quality relies on the
/// structured-output `analysis` field (see `commit_agent::commit_schema`) as the
/// model's reasoning channel.
const MODEL: ModelSpec = ModelSpec {
    id: "qwen3.5-2b",
    repo: "bartowski/Qwen_Qwen3.5-2B-GGUF",
    file: "Qwen_Qwen3.5-2B-Q4_K_M.gguf",
    sha256: Some("57a1085840f497d764a7fc5d346922dbde961efb54cc792ea81d694fd846a1d8"), // 1.40 GB
    ctx: 8192,
};

fn active_spec() -> &'static ModelSpec {
    &MODEL
}

/// Public view of model readiness, for the UI's first-run / download messaging.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// The active model's id.
    pub(crate) model: String,
    /// The GGUF is present on disk (so generation won't trigger a download).
    pub(crate) downloaded: bool,
    /// On-disk size in MB (when present).
    pub(crate) size_mb: Option<u64>,
    /// Where the GGUF lives / will be cached.
    pub(crate) path: String,
}

pub fn model_status() -> ModelStatus {
    let spec = active_spec();
    let path = model_path(spec);
    let (downloaded, size_mb) = match path.as_ref().ok().and_then(|p| std::fs::metadata(p).ok()) {
        Some(m) => (true, Some(m.len() / 1_000_000)),
        None => (false, None),
    };
    ModelStatus {
        model: spec.id.to_string(),
        downloaded,
        size_mb,
        path: path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

// ───────────────────────────── model on disk ─────────────────────────────

/// Directory the GGUF is cached in: `$VIBESTUDIO_MODEL_DIR` or the per-OS data
/// dir (`~/Library/Application Support`, `%APPDATA%`, `$XDG_DATA_HOME`) under
/// `vibestudio/models`.
fn model_dir() -> Result<PathBuf, String> {
    if let Ok(d) = std::env::var("VIBESTUDIO_MODEL_DIR") {
        if !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    let base = dirs::data_dir().ok_or_else(|| "Cannot locate a data directory for models.".to_string())?;
    Ok(base.join("vibestudio").join("models"))
}

fn model_path(spec: &ModelSpec) -> Result<PathBuf, String> {
    Ok(model_dir()?.join(spec.file))
}

/// Resolve the model to a usable local GGUF, downloading it on first use.
/// A local override (`VIBESTUDIO_COMMIT_MODEL_PATH`) wins — for airgapped /
/// WSL2 installs that import a `.gguf` manually.
fn ensure_model() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("VIBESTUDIO_COMMIT_MODEL_PATH") {
        if !p.is_empty() {
            let path = PathBuf::from(p);
            return if path.exists() {
                Ok(path)
            } else {
                Err(format!("VIBESTUDIO_COMMIT_MODEL_PATH does not exist: {}", path.display()))
            };
        }
    }
    let spec = active_spec();
    let path = model_path(spec)?;
    if path.exists() {
        return Ok(path);
    }
    // Serialize downloads: a startup prefetch and an on-demand generate can reach
    // here at the same time — without this lock they'd both fetch the same ~1.4 GB
    // file. Whoever loses the race re-checks and finds it already in place.
    static DOWNLOAD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _dl = DOWNLOAD_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Model download is unavailable (lock poisoned).".to_string())?;
    if path.exists() {
        return Ok(path); // another thread finished the download while we waited
    }
    download_model(spec, &path)?;
    Ok(path)
}

/// Kick off the model download in the background if it isn't present yet, so the
/// first generation doesn't stall on a ~1.4 GB fetch. Fire-and-forget and
/// idempotent (downloads are serialized in `ensure_model`) — call once at startup.
/// Only downloads; the server is still spawned lazily on first `chat`, so this
/// never loads the model into memory until it's actually used.
pub fn prefetch_model() {
    // The on-device model is opt-in only — the default path shells out to a
    // logged-in coding-agent CLI (see `commit_agent`). Never fetch the ~1.4 GB
    // GGUF unless the user explicitly selected the offline backend.
    if !crate::commit_agent::offline_opted_in() {
        return;
    }
    if model_status().downloaded {
        return; // already cached (or a local override is in use)
    }
    std::thread::spawn(|| {
        if let Err(e) = ensure_model() {
            // Don't fail startup; the on-demand path surfaces the error in the UI.
            // Now RUST_LOG-gated (shown by default) instead of needing a debug env var.
            log::warn!("model prefetch failed: {e}");
        }
    });
}

/// Stream the GGUF to a `.part` file, verify SHA-256 (when pinned), then
/// atomically rename into place so a partial download is never used.
fn download_model(spec: &ModelSpec, dest: &PathBuf) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let dir = dest.parent().ok_or_else(|| "Bad model path.".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create model dir: {e}"))?;

    let url = format!("https://huggingface.co/{}/resolve/main/{}", spec.repo, spec.file);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| format!("Couldn't download the model ({}): {e}", spec.id))?;

    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| format!("Couldn't write model file: {e}"))?;
    let mut hasher = Sha256::new();
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("Download interrupted: {e}")
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut out, &buf[..n]).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            format!("Couldn't write model file: {e}")
        })?;
    }
    drop(out);

    if let Some(expected) = spec.sha256 {
        let got = hasher.finalize();
        let got_hex = got.iter().map(|b| format!("{b:02x}")).collect::<String>();
        if !got_hex.eq_ignore_ascii_case(expected) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("Model checksum mismatch (expected {expected}, got {got_hex}). Refusing to use it."));
        }
    }
    std::fs::rename(&part, dest).map_err(|e| format!("Couldn't finalize model file: {e}"))?;
    Ok(())
}

// ──────────────────────────── server process ────────────────────────────

/// How many layers to offload to GPU. `auto` lets llama-server choose based on
/// available VRAM — paired with the default `--fit on`, which trims layers (down
/// to `--fit-ctx`) to leave a per-device margin — and resolves to 0 (pure CPU)
/// when no GPU backend is loaded. Only passed on the GPU attempt; the CPU path
/// uses `"0"` so nothing offloads even if ggml auto-loads a sibling CUDA backend.
const GPU_NGL: &str = "auto";
/// How long to wait for the server's `/health` to report ready (model load).
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// Tighter deadline for the GPU attempt: a working GPU load is quick, so if it
/// stalls (driver/VRAM trouble) we fail over to the CPU spawn — which gets the
/// full `READY_TIMEOUT` — promptly, instead of burning ~3 min before falling back.
const GPU_READY_TIMEOUT: Duration = Duration::from_secs(60);

struct Engine {
    child: Child,
    port: u16,
}

impl Engine {
    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn engine() -> &'static Mutex<Option<Engine>> {
    static ENGINE: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Resolve the `llama-server` executable, in priority order:
///   1. `VIBESTUDIO_LLAMA_SERVER` (explicit override; the Tauri app also sets
///      this to the bundled resource path on the user's machine).
///   2. Next to our own binary (where a Tauri sidecar would land).
///   3. The repo's vendored tree `<repo>/client/desktop/binaries/<triple>/` — covers
///      every dev invocation (skill-server, `tauri dev`, `cargo test`) with no
///      env var. In a shipped build this baked-in path doesn't exist, so it's
///      skipped and (1) carries it.
///   4. `PATH`.
pub(crate) fn engine_binary() -> PathBuf {
    if let Ok(p) = std::env::var("VIBESTUDIO_LLAMA_SERVER") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let exe_name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(exe_name);
            if cand.exists() {
                return cand;
            }
        }
    }
    // skill-core's manifest is server/skill-core, so the repo root is two up.
    let vendored = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/desktop/binaries");
    if let Some(p) = find_in_dir(&vendored, exe_name) {
        return p;
    }
    PathBuf::from(exe_name) // fall back to PATH lookup
}

/// Ensure the engine binary is executable — bundling it as an app resource can
/// strip the exec bit on unix, which would make the spawn fail with EACCES.
fn ensure_executable(bin: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(bin) {
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                let _ = std::fs::set_permissions(bin, std::fs::Permissions::from_mode(mode | 0o755));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = bin;
}

/// Find `<base>/<subdir>/<exe>` (one platform subdir) or `<base>/<exe>`.
fn find_in_dir(base: &std::path::Path, exe: &str) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(base) {
        for e in entries.flatten() {
            let cand = e.path().join(exe);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let direct = base.join(exe);
    direct.is_file().then_some(direct)
}

/// Prepend directories to the platform's dynamic-library search path for the
/// spawned child, so llama.cpp's sibling shared libs (and, for GPU, the user's
/// CUDA runtime) always load.
fn add_lib_paths(cmd: &mut Command, dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let joined = dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(sep);
    let var = if cfg!(windows) {
        "PATH"
    } else if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let prev = std::env::var(var).unwrap_or_default();
    cmd.env(var, if prev.is_empty() { joined } else { format!("{joined}{sep}{prev}") });
}

/// Bind to port 0 to let the OS pick a free port, then release it for the child.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("No free port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok(port) // listener dropped here → port freed for llama-server
}

fn spawn_one(
    model: &Path,
    ctx: u32,
    ngl: &str,
    gpu: Option<&crate::gpu::GpuBackend>,
    ready_timeout: Duration,
) -> Result<Engine, String> {
    let port = free_port()?;
    let bin = engine_binary();
    ensure_executable(&bin); // bundling as a resource can drop the exec bit on unix
    let mut cmd = hidden_command(&bin);
    cmd.args([
        "-m",
        &model.to_string_lossy(),
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "-c",
        &ctx.to_string(),
        "-ngl",
        ngl,
        "--jinja", // use the model's embedded chat template (so enable_thinking is honored)
    ])
    .stdout(Stdio::null())
    // Surface llama-server's own logs when debugging; silent otherwise.
    .stderr(if std::env::var_os("VIBESTUDIO_COMMIT_DEBUG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    });
    // Build the child's dynamic-library search path: the engine's own dir (sibling
    // ggml libs; rpath=$ORIGIN usually covers it, but be explicit) plus, when a GPU
    // backend was selected, the user's CUDA runtime dirs so the bridge can resolve
    // libcudart/libcublas. `gpu` is Some only on the acceleration attempt — the
    // caller decides; the CPU attempt passes None so nothing offloads.
    let mut lib_dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = bin.parent() {
        if !dir.as_os_str().is_empty() {
            lib_dirs.push(dir.to_path_buf());
        }
    }
    if let Some(g) = gpu {
        cmd.env("GGML_BACKEND_PATH", &g.backend_lib);
        lib_dirs.extend(g.lib_dirs.iter().cloned());
    }
    add_lib_paths(&mut cmd, &lib_dirs);
    let child = cmd.spawn().map_err(|e| {
        format!(
            "Couldn't start the local AI engine (llama-server): {e}. \
             It should ship with the app; for a dev build run scripts/fetch-engine.sh, \
             or set VIBESTUDIO_LLAMA_SERVER to a llama-server path."
        )
    })?;

    let mut engine = Engine { child, port };
    match wait_ready(&mut engine, ready_timeout) {
        Ok(()) => Ok(engine),
        Err(e) => {
            drop(engine); // kills the child
            Err(e)
        }
    }
}

/// Spawn the engine, deciding GPU-vs-CPU up front via `gpu::gpu_plan()`:
///   - macOS (`BuiltIn`): `-ngl auto` so the engine's compiled-in Metal backend is
///     used — no bridge.
///   - NVIDIA Linux/Windows (`Cuda`): `-ngl auto` plus the bundled CUDA bridge and
///     the user's CUDA runtime on the loader path.
///   - everything else (`Cpu`: GPU disabled, non-NVIDIA, no CUDA runtime): spawn
///     straight to `-ngl 0` — authoritative even if ggml auto-loads a sibling CUDA
///     backend, since 0 layers offload.
///
/// The GPU attempts run under a tight timeout and fall back to CPU; a pure-CPU
/// failure surfaces immediately rather than re-running an identical spawn.
fn spawn_engine(model: &Path, ctx: u32) -> Result<Engine, String> {
    match crate::gpu::gpu_plan() {
        crate::gpu::GpuPlan::Cpu => spawn_one(model, ctx, "0", None, READY_TIMEOUT),
        crate::gpu::GpuPlan::BuiltIn => spawn_one(model, ctx, GPU_NGL, None, GPU_READY_TIMEOUT)
            .or_else(|_| spawn_one(model, ctx, "0", None, READY_TIMEOUT)),
        crate::gpu::GpuPlan::Cuda(gpu) => {
            spawn_one(model, ctx, GPU_NGL, Some(&gpu), GPU_READY_TIMEOUT)
                .or_else(|_| spawn_one(model, ctx, "0", None, READY_TIMEOUT))
        }
    }
}

/// Poll `/health` until it returns 200 (only then is the model loaded and ready)
/// or the child dies / we hit `timeout`.
fn wait_ready(engine: &mut Engine, timeout: Duration) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/health", engine.port);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(500))
        .timeout_read(Duration::from_secs(2))
        .build();
    let deadline = Instant::now() + timeout;
    loop {
        if engine.exited() {
            return Err("The local AI engine exited before it was ready.".into());
        }
        if agent.get(&url).call().is_ok() {
            return Ok(()); // 200 → model loaded (503 while loading comes back as Err)
        }
        if Instant::now() >= deadline {
            return Err("The local AI engine didn't become ready in time.".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ─────────────────────────────── chat call ───────────────────────────────

/// One OpenAI-style chat message.
#[derive(Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        ChatMessage { role: role.to_string(), content: content.into() }
    }
}

/// Send a chat completion to the (lazily started, warm-kept) engine and return
/// the assistant's text. Serialized through a global lock — one generation at a
/// time, which is all a single-user desktop needs.
/// `response_format`, when set, is forwarded verbatim (e.g. an OpenAI-style
/// `{"type":"json_schema","json_schema":{…}}`). llama-server compiles the schema
/// into a GBNF grammar and constrains the output to valid JSON — we use this to
/// make the model produce an `analysis` field before the `commit_message`.
pub fn chat(
    messages: &[ChatMessage],
    temperature: f32,
    seed: i64,
    response_format: Option<serde_json::Value>,
) -> Result<String, String> {
    let model = ensure_model()?;
    let ctx = active_spec().ctx;

    let mut guard = engine().lock().map_err(|_| "AI engine state is unavailable.".to_string())?;
    if guard.as_mut().map(|e| e.exited()).unwrap_or(true) {
        *guard = Some(spawn_engine(&model, ctx)?);
    }
    let port = guard.as_ref().unwrap().port;

    #[derive(Serialize)]
    struct ChatRequest<'a> {
        messages: &'a [ChatMessage],
        temperature: f32,
        /// Sampling seed. A fixed seed (the auto/eager path) makes the same diff
        /// produce the same message; a varying seed (the manual re-roll) gives a
        /// fresh phrasing each click.
        seed: i64,
        stream: bool,
        /// Forwarded to the model's chat template. Qwen3 defaults thinking ON,
        /// which buries the answer in a <think> block and leaves `content` empty —
        /// fatal for terse commit messages. Disabling it yields the message
        /// directly. Harmless for non-thinking models (the kwarg is just unused).
        chat_template_kwargs: serde_json::Value,
        /// JSON-schema / grammar constraint (omitted when None).
        #[serde(skip_serializing_if = "Option::is_none")]
        response_format: Option<serde_json::Value>,
    }
    // No `max_tokens`: output is bounded only by the context window, EOS, and the
    // JSON grammar (the model stops once it closes the schema object). An absent cap
    // means llama-server runs with n_predict = -1 (generate until context/EOS).
    let body = serde_json::to_string(&ChatRequest {
        messages,
        temperature,
        seed,
        stream: false,
        chat_template_kwargs: serde_json::json!({ "enable_thinking": false }),
        response_format,
    })
    .map_err(|e| format!("Couldn't build the AI request: {e}"))?;

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(180))
        .build();
    let resp = agent
        .post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string(&body);

    // A dead/broken server can leave a stale handle — drop it so the next call respawns.
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            *guard = None;
            return Err(format!("The local AI engine failed to respond: {e}"));
        }
    };

    let text = resp.into_string().map_err(|e| format!("Couldn't read the AI response: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Unexpected AI response: {e}"))?;
    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "The AI returned an empty response.".to_string())
}

/// Reap the engine child (call on app/server exit so nothing is orphaned).
pub fn shutdown() {
    if let Ok(mut guard) = engine().lock() {
        *guard = None; // Drop kills + waits the child
    }
}

/// Kill any stray `llama-server` left over from a previous run that was
/// hard-killed before it could reap its engine (e.g. a `tauri dev` rebuild that
/// SIGKILLs the app). Matches OUR resolved engine binary path, so it never
/// touches an unrelated llama-server the user runs — and ONLY processes that
/// are actually orphaned (reparented to init): a live engine is the direct
/// child of the backend that spawned it, so a second backend starting up must
/// never shoot down the first one's engine mid-generation.
pub fn reap_orphans() {
    // Only the opt-in offline backend ever spawns an engine; skip the scan
    // entirely on the default (CLI) path so we never touch a stray process.
    if !crate::commit_agent::offline_opted_in() {
        return;
    }
    let needle = engine_binary().to_string_lossy().into_owned();
    if needle.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        let Ok(out) = hidden_command("ps").args(["-eo", "pid=,ppid=,args="]).output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let me = std::process::id();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut parts = line.split_whitespace();
            let (Some(pid_s), Some(ppid_s)) = (parts.next(), parts.next()) else {
                continue;
            };
            let args = parts.collect::<Vec<_>>().join(" ");
            if !args.contains(&needle) {
                continue;
            }
            let (Ok(pid), Ok(ppid)) = (pid_s.parse::<u32>(), ppid_s.parse::<u32>()) else {
                continue;
            };
            // ppid 1 = reparented to init ⇒ its backend is gone. (Under a
            // subreaper the orphan reparents elsewhere and is missed — the
            // conservative direction: we'd rather leave a stray engine than
            // kill a sibling backend's live one.)
            if pid != me && ppid == 1 {
                let _ = hidden_command("kill").arg(pid.to_string()).output(); // SIGTERM
            }
        }
    }
    #[cfg(windows)]
    {
        // Best-effort on Windows: kill our bundled engine by image name. No
        // cheap parent check here, so this CAN hit a sibling backend's engine —
        // acceptable for the rare run-two-backends-on-Windows case.
        let _ = hidden_command("taskkill").args(["/F", "/IM", "llama-server.exe"]).output();
    }
}
