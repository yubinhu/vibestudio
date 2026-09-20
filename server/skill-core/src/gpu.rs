//! Optional GPU acceleration for the on-device engine: **CUDA if the user has it,
//! otherwise CPU.**
//!
//! `llama-server` offloads to a GPU only if a matching ggml *backend* library is
//! present (it's a `GGML_BACKEND_DL` build). We bundle a small `libggml-cuda.so`
//! bridge next to the engine, but we deliberately do **not** ship the ~1 GB CUDA
//! runtime (cuBLAS/cudart) — the bridge links the user's own CUDA install. So:
//!   - user has the NVIDIA driver **and** the CUDA runtime → bridge loads → **GPU**;
//!   - user has only the driver (or no NVIDIA) → bridge can't load → **CPU**.
//!
//! This module's job is to (a) detect an NVIDIA GPU, (b) find the bundled bridge,
//! and (c) discover where the user's CUDA runtime libs live so the spawned engine
//! can resolve them. The actual "use it or fall back" is enforced by `llama-server`:
//! a missing runtime makes the backend load fail (non-fatal) and it runs on CPU.
//!
//! CUDA-only by design (per product decision): no Vulkan/ROCm. macOS needs nothing
//! here — its build has Metal compiled in, so `-ngl auto` uses the GPU directly.
//! `VIBESTUDIO_DISABLE_GPU=1` forces CPU; `VIBESTUDIO_CUDA_DIR=<dir[:dir...]>`
//! points at a CUDA runtime in a non-standard location.

use std::path::{Path, PathBuf};

/// A usable GPU backend: the ggml CUDA bridge to load, plus directories the
/// spawned engine must put on its dynamic-library search path so the bridge can
/// resolve the user's CUDA runtime (libcudart/libcublas).
pub struct GpuBackend {
    /// Path to the bundled `libggml-cuda.so` / `ggml-cuda.dll`.
    pub backend_lib: PathBuf,
    /// Extra lib directories (the user's CUDA runtime). May be empty when the
    /// runtime is already on the system loader path.
    pub lib_dirs: Vec<PathBuf>,
}

/// How to run the engine's GPU offload.
pub enum GpuPlan {
    /// CPU only — pass `-ngl 0` (GPU disabled, or no usable GPU on this platform).
    Cpu,
    /// GPU via the engine's *built-in* backend — `-ngl auto`, no bridge. macOS:
    /// Metal is compiled into the prebuilt, so `-ngl auto` uses it directly.
    BuiltIn,
    /// GPU via the bundled CUDA bridge — `-ngl auto`, load the bridge + the user's
    /// CUDA runtime dirs. Linux/Windows with an NVIDIA GPU.
    Cuda(GpuBackend),
}

/// Decide how to run: built-in Metal on macOS, CUDA on Linux/Windows when an NVIDIA
/// GPU + bridge are present, otherwise CPU. `VIBESTUDIO_DISABLE_GPU=1` forces CPU
/// everywhere (incl. Metal). Cheap and side-effect free (filesystem checks + one
/// quick `nvidia-smi`), so it's safe on the spawn path — no downloads, never blocks.
pub fn gpu_plan() -> GpuPlan {
    if std::env::var_os("VIBESTUDIO_DISABLE_GPU").is_some() {
        return GpuPlan::Cpu;
    }
    // macOS ships Metal *inside* the engine — there's no separate backend to load;
    // `-ngl auto` offloads to it. (We never bundle a CUDA bridge for macOS.)
    if cfg!(target_os = "macos") {
        return GpuPlan::BuiltIn;
    }
    match cuda_backend() {
        Some(b) => GpuPlan::Cuda(b),
        None => GpuPlan::Cpu,
    }
}

/// The bundled CUDA bridge + the user's CUDA runtime dirs, when usable (NVIDIA GPU
/// present and the bridge is bundled). `None` → no CUDA GPU. Linux/Windows only.
fn cuda_backend() -> Option<GpuBackend> {
    let lib_name = backend_lib_name()?; // None where we ship no CUDA bridge (macOS)
    if !nvidia_present() {
        return None;
    }
    let backend_lib = find_backend_lib(lib_name)?; // bundled next to the engine binary
    Some(GpuBackend { backend_lib, lib_dirs: cuda_runtime_dirs() })
}

/// The ggml CUDA backend filename for this platform, or `None` where we don't ship
/// one (macOS uses built-in Metal; other targets unsupported).
fn backend_lib_name() -> Option<&'static str> {
    match std::env::consts::OS {
        "linux" => Some("libggml-cuda.so"),
        "windows" => Some("ggml-cuda.dll"),
        _ => None,
    }
}

/// Is there an NVIDIA GPU? Primary signal is `nvidia-smi -L`; but it isn't always
/// on PATH even when the driver is (containers, some WSL2/minimal setups, a
/// stripped child PATH), so fall back to the presence of the driver lib itself.
/// A false positive just means the bridge load fails non-fatally → CPU.
fn nvidia_present() -> bool {
    nvidia_smi_reports_gpu() || driver_lib_present()
}

fn nvidia_smi_reports_gpu() -> bool {
    let exe = if cfg!(windows) { "nvidia-smi.exe" } else { "nvidia-smi" };
    match crate::process::hidden_command(exe).arg("-L").output() {
        Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).contains("GPU "),
        Err(_) => false,
    }
}

/// The NVIDIA driver library (not the toolkit stub) — present iff a driver is
/// installed. Covers standard Linux, WSL2, and Windows locations.
fn driver_lib_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        [
            "/usr/lib/x86_64-linux-gnu/libcuda.so.1",
            "/usr/lib/wsl/lib/libcuda.so.1",
            "/usr/lib64/libcuda.so.1",
            "/usr/lib/aarch64-linux-gnu/libcuda.so.1",
        ]
        .iter()
        .any(|p| Path::new(p).exists())
    }
    #[cfg(target_os = "windows")]
    {
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| String::from("C:\\Windows"));
        return Path::new(&sysroot).join("System32").join("nvcuda.dll").exists();
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

/// Find the bundled CUDA bridge: it ships next to the engine binary (alongside the
/// other `libggml-*` libs), where ggml's dynamic loader also looks.
fn find_backend_lib(lib_name: &str) -> Option<PathBuf> {
    let bin = crate::engine::engine_binary();
    let cand = bin.parent()?.join(lib_name);
    cand.is_file().then_some(cand)
}

/// Directories holding the user's CUDA runtime (`libcudart.so.12`, `libcublas.so.12`),
/// to add to the engine's loader path. Empty when the runtime is already system-
/// resolvable (the bridge will find it without help) or absent (→ CPU fallback).
fn cuda_runtime_dirs() -> Vec<PathBuf> {
    // Explicit override wins (airgapped / non-standard installs).
    if let Ok(v) = std::env::var("VIBESTUDIO_CUDA_DIR") {
        let dirs: Vec<PathBuf> = std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect();
        if !dirs.is_empty() {
            return dirs;
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |d: PathBuf| {
        if !dirs.contains(&d) && dir_has_cuda_runtime(&d) {
            dirs.push(d);
        }
    };

    // System toolkit / package locations.
    add(PathBuf::from("/usr/local/cuda/lib64"));
    add(PathBuf::from("/usr/local/cuda/targets/x86_64-linux/lib"));
    add(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
    if let Ok(rd) = std::fs::read_dir("/usr/local") {
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with("cuda-") {
                add(e.path().join("lib64"));
            }
        }
    }

    // pip wheels (`nvidia-cuda-runtime-cu12`, `nvidia-cublas-cu12`) — common for ML
    // devs; cudart and cublas land in *separate* nvidia/*/lib dirs. Cover `--user`
    // (~/.local) AND the active virtualenv/conda env, where most devs install.
    let mut wheel_roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        wheel_roots.push(home.join(".local"));
    }
    for env_var in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Ok(prefix) = std::env::var(env_var) {
            if !prefix.is_empty() {
                wheel_roots.push(PathBuf::from(prefix));
            }
        }
    }
    for root in &wheel_roots {
        for d in nvidia_wheel_lib_dirs(root) {
            add(d);
        }
    }

    // Windows CUDA toolkit: the runtime DLLs live in <CUDA_PATH>\bin (usually also
    // on PATH, but be explicit). Cover the standard install location too.
    #[cfg(windows)]
    {
        if let Ok(cuda_path) = std::env::var("CUDA_PATH") {
            add(PathBuf::from(cuda_path).join("bin"));
        }
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| String::from("C:\\Program Files"));
        let cuda_root = PathBuf::from(pf).join("NVIDIA GPU Computing Toolkit").join("CUDA");
        if let Ok(rd) = std::fs::read_dir(&cuda_root) {
            for e in rd.flatten() {
                add(e.path().join("bin"));
            }
        }
    }

    dirs
}

/// `<prefix>/lib/<python*>/site-packages/nvidia/<pkg>/lib` dirs — where the
/// nvidia-*-cu12 wheels drop libcudart/libcublas (each in its own pkg dir).
fn nvidia_wheel_lib_dirs(prefix: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(pys) = std::fs::read_dir(prefix.join("lib")) else {
        return out;
    };
    for py in pys.flatten() {
        let nvidia = py.path().join("site-packages").join("nvidia");
        if let Ok(pkgs) = std::fs::read_dir(&nvidia) {
            for pkg in pkgs.flatten() {
                out.push(pkg.path().join("lib"));
            }
        }
    }
    out
}

/// Does this directory contain a CUDA runtime lib the bridge needs?
fn dir_has_cuda_runtime(d: &Path) -> bool {
    if !d.is_dir() {
        return false;
    }
    ["libcudart.so.12", "libcublas.so.12", "cudart64_12.dll", "cublas64_12.dll"]
        .iter()
        .any(|f| d.join(f).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_name_matches_platform() {
        match std::env::consts::OS {
            "linux" => assert_eq!(backend_lib_name(), Some("libggml-cuda.so")),
            "windows" => assert_eq!(backend_lib_name(), Some("ggml-cuda.dll")),
            _ => assert_eq!(backend_lib_name(), None), // macOS → Metal, no bridge fetched
        }
    }

    #[test]
    fn disabled_env_forces_cpu() {
        if std::env::var_os("VIBESTUDIO_DISABLE_GPU").is_some() {
            assert!(matches!(gpu_plan(), GpuPlan::Cpu));
            return;
        }
        // Set the override only in a child so parallel tests retain their env.
        let output = crate::process::hidden_command(std::env::current_exe().unwrap())
            .args(["--exact", "gpu::tests::disabled_env_forces_cpu", "--nocapture"])
            .env("VIBESTUDIO_DISABLE_GPU", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "GPU override probe failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_builtin_metal() {
        // Metal is compiled into the macOS engine: the plan must be BuiltIn (so we
        // pass `-ngl auto`), never Cpu, unless GPU is explicitly disabled.
        if std::env::var_os("VIBESTUDIO_DISABLE_GPU").is_none() {
            assert!(matches!(gpu_plan(), GpuPlan::BuiltIn));
        }
    }

    #[test]
    fn cuda_dir_detection_is_precise() {
        // A dir with no CUDA libs is never selected.
        assert!(!dir_has_cuda_runtime(Path::new("/")));
        assert!(!dir_has_cuda_runtime(Path::new("/nonexistent-xyz")));
    }
}
