//! The desktop connects to a durable, per-user local host service. This module
//! owns only discovery and recovery; closing the desktop never stops the worker.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use skill_server::host_service::{self, HostServiceOptions, HostServiceRecord};
use skill_server::{LocalBackendControl, RemoteTarget};

pub struct LocalHost {
    options: HostServiceOptions,
    current: Mutex<Result<HostServiceRecord, String>>,
    recovery: Mutex<()>,
    stopped: AtomicBool,
}

impl LocalHost {
    pub fn resume_after_failed_update(&self) -> Result<(), String> {
        let _recovery = self.recovery.lock().unwrap();
        let result = host_service::ensure(&self.options);
        *self.current.lock().unwrap() = result.clone();
        self.stopped.store(false, Ordering::SeqCst);
        result.map(|_| ())
    }

    pub fn stop(&self) -> Result<(), String> {
        self.stopped.store(true, Ordering::SeqCst);
        // Finish an already-running ensure before stopping its resulting worker.
        // The routing snapshot has its own lock and remains nonblocking.
        let _recovery = self.recovery.lock().unwrap();
        let record = self.current.lock().unwrap().clone();
        let result = record.and_then(|record| host_service::stop(&record));
        if result.is_err() {
            self.stopped.store(false, Ordering::SeqCst);
        }
        result
    }

    pub fn start(options: HostServiceOptions) -> Result<Arc<Self>, String> {
        let record = host_service::ensure(&options)?;
        let host = Arc::new(Self {
            options: options.clone(),
            current: Mutex::new(Ok(record)),
            recovery: Mutex::new(()),
            stopped: AtomicBool::new(false),
        });
        let observer = host.clone();
        let recovery_options = HostServiceOptions {
            recovery: true,
            ..options
        };
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(3));
            let _recovery = observer.recovery.lock().unwrap();
            if observer.stopped.load(Ordering::SeqCst) {
                continue;
            }
            let current = observer.current.lock().unwrap().clone();
            if current.as_ref().is_ok_and(host_service::healthy) {
                continue;
            }
            *observer.current.lock().unwrap() =
                Err("Reconnecting to the local host service…".into());
            let result = host_service::ensure(&recovery_options);
            if let Err(error) = &result {
                if current.as_ref().err() != Some(error) {
                    log::warn!("local host service recovery: {error}");
                }
            }
            *observer.current.lock().unwrap() = result;
        });
        Ok(host)
    }
}

impl LocalBackendControl for LocalHost {
    fn target(&self) -> Result<RemoteTarget, String> {
        self.current
            .lock()
            .unwrap()
            .as_ref()
            .map(|record| RemoteTarget {
                base_url: record.base_url(),
                token: String::new(),
            })
            .map_err(Clone::clone)
    }
}
