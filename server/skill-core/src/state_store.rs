//! Small JSON stores for workspace preferences and history. A separate lock file
//! serializes read/modify/write across desktop and standalone server processes.
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub(crate) fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("Could not read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn update<T: DeserializeOwned + Default + Serialize>(
    path: &Path,
    change: impl FnOnce(&mut T) -> Result<(), String>,
) -> Result<T, String> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    // On corrupt/unreadable data, refuse to overwrite the user's existing file.
    let mut value = read(path)?;
    change(&mut value)?;
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = options
            .truncate(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result?;
    // Dropping the handle releases the lock, including on early error returns.
    Ok(value)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct TempDir(pub PathBuf);
    impl TempDir {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "vibestudio-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_updates_do_not_lose_entries() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        std::thread::scope(|scope| {
            for n in 0..20 {
                let path = &path;
                scope.spawn(move || {
                    update::<Vec<usize>>(path, |items| {
                        items.push(n);
                        Ok(())
                    })
                    .unwrap()
                });
            }
        });
        let items: Vec<usize> = read(&path).unwrap();
        assert_eq!(items.len(), 20);
    }

    #[test]
    fn corrupt_data_is_preserved() {
        let dir = TempDir::new();
        let path = dir.0.join("state.json");
        fs::write(&path, "broken json").unwrap();
        assert!(update::<Vec<usize>>(&path, |_| Ok(())).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "broken json");
    }
}
