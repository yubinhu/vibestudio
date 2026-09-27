//! Discover the ephemeral desktop listener without guessing the host-service port.
use std::path::PathBuf;

pub struct EndpointRecord(PathBuf);

impl EndpointRecord {
    pub fn publish(port: u16) -> Result<Self, String> {
        let path = skill_core::paths::ensure_config_dir()?.join("comparison-desktop.json");
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let value = serde_json::json!({
            "protocol": 1,
            "pid": std::process::id(),
            "baseUrl": format!("http://127.0.0.1:{port}")
        });
        std::fs::write(&temp, serde_json::to_vec(&value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        // Windows rename cannot replace an existing destination. This record is
        // only a discovery hint; callers verify the listener before sending work.
        #[cfg(windows)]
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }

    pub fn remove(&self) {
        let ours = std::fs::read(&self.0).ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|value| value["pid"].as_u64() == Some(std::process::id() as u64));
        if ours { let _ = std::fs::remove_file(&self.0); }
    }
}

impl Drop for EndpointRecord {
    fn drop(&mut self) { self.remove(); }
}
