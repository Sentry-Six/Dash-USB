//! Setup progress and phase events. Stable phase IDs persist across rebooted
//! WebSocket sessions; no-op phases remain unannounced.

use std::sync::Arc;

#[derive(Clone)]
pub struct SetupEmitter {
    progress: Arc<dyn Fn(&str) + Send + Sync>,
    phase: Arc<dyn Fn(&str, &str) + Send + Sync>,
}

impl SetupEmitter {
    pub fn new(
        progress: impl Fn(&str) + Send + Sync + 'static,
        phase: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            progress: Arc::new(progress),
            phase: Arc::new(phase),
        }
    }

    /// Writes to the log file and broadcasts to WebSocket clients.
    pub fn progress(&self, msg: &str) {
        (self.progress)(msg);
    }

    /// Announce work with a reboot-stable ID and user-facing label.
    pub fn begin_phase(&self, id: &str, label: &str) {
        (self.phase)(id, label);
    }
}
