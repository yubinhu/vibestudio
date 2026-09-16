//! Native app authentication gate. The shell owns authentication and its grace
//! period; HTTP clients can observe denial but can never unlock the app.
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AppAccess {
    unlocked: AtomicBool,
}

impl AppAccess {
    /// Install this gate before starting a phone's loopback listener.
    pub fn new_locked() -> Self {
        Self { unlocked: AtomicBool::new(false) }
    }

    pub fn is_unlocked(&self) -> bool {
        self.unlocked.load(Ordering::Acquire)
    }

    /// Only the native authentication controller calls this, never an API route.
    pub fn set_unlocked(&self, unlocked: bool) {
        self.unlocked.store(unlocked, Ordering::Release);
    }
}
