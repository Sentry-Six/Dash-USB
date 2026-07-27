//! Setup event emitter. Two channels:
//!
//! * `progress(msg)`: free-form log lines, streamed to the log file and
//!   broadcast as `setup_progress` WebSocket events.
//! * `begin_phase(id, label)`: a user-visible phase started doing actual work.
//!   Phases that no-op (e.g. an already-configured dwc2 overlay on re-run)
//!   never announce themselves, so the UI only lists phases actually executed
//!   this run.
//!
//! The phase callback also persists the phase to
//! `/dashusb/setup-phases.jsonl` so the UI can reconstruct the list across a
//! reboot-triggered WebSocket disconnect.

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

    /// Announce the start of a user-visible phase.
    ///
    /// Call only when the phase will actually do work; this drives the
    /// wizard's live phase list. `id` must be stable across reboots for the UI
    /// to deduplicate. `label` is shown to the user.
    pub fn begin_phase(&self, id: &str, label: &str) {
        (self.phase)(id, label);
    }
}
