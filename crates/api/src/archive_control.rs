//! One-cycle archive cancellation and durable cleanup protection.
use std::{io, path::{Path, PathBuf}};

#[derive(Clone)]
pub struct ArchiveControl {
    directory: PathBuf,
    cleanup_deferred: PathBuf,
}

impl Default for ArchiveControl {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("/tmp"),
            cleanup_deferred: PathBuf::from("/mutable/dashusb_cam_cleanup_deferred"),
        }
    }
}

impl ArchiveControl {
    pub fn mark_failed(&self) {
        if let Some(id) = self.active_cycle() {
            let _ = std::fs::write(self.directory.join(format!("archive-stage-failed-{id}")), b"worker error\n");
        }
    }
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().to_owned(),
            cleanup_deferred: directory.as_ref().join("dashusb_cam_cleanup_deferred"),
        }
    }

    pub fn active_cycle(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.directory.join("archive-cycle")).ok()?;
        let id = id.trim();
        let (pid, nonce) = id.split_once(':')?;
        let pid: u32 = pid.parse().ok()?;
        if nonce.is_empty() || id.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        // An abrupt service exit must not leave a phantom Cancel button.
        #[cfg(target_os = "linux")]
        if !Path::new(&format!("/proc/{pid}")).exists() { return None; }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        Some(id.to_owned())
    }

    pub fn cancelled(&self) -> bool {
        let Some(id) = self.active_cycle() else { return false; };
        self.cycle_cancelled(&id)
    }

    pub fn cycle_cancelled(&self, id: &str) -> bool {
        self.directory.join(format!("archive-cycle-cancel-{id}")).exists()
    }

    /// Hold the returned guard until companion workers have been notified.
    /// The shell cannot close this cycle and start another while it is held.
    pub fn request_cancel(&self, expected_cycle: &str) -> io::Result<crate::archive_mount_lock::ArchiveMountGuard> {
        // Shared with shell cycle-close and cleanup-deferral release. An
        // accepted request cannot race past cycle close or lose its durable
        // protection to a simultaneously successful transfer.
        let guard = crate::archive_mount_lock::acquire_path(
            &self.directory.join("archive-cycle.lock"),
            std::time::Duration::from_secs(5),
        )?;
        if self.active_cycle().as_deref() != Some(expected_cycle) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "archive cycle has already ended"));
        }
        // Persist protection BEFORE the volatile request is visible or HTTP
        // 202 is returned. File and directory fsync also cover sudden power loss.
        let persist = || -> io::Result<()> {
            let file = std::fs::OpenOptions::new()
                .write(true).create(true).truncate(false).open(&self.cleanup_deferred)?;
            file.sync_all()?;
            #[cfg(unix)]
            std::fs::File::open(self.cleanup_deferred.parent().unwrap())?.sync_all()?;
            Ok(())
        };
        persist().map_err(|e| io::Error::other(format!("cannot protect footage cleanup: {e}")))?;
        // Different cycles have different files. A request delayed across a
        // cycle transition cannot overwrite the newer cycle's cancellation.
        std::fs::write(self.directory.join(format!("archive-cycle-cancel-{expected_cycle}")), b"")?;
        Ok(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(control: &ArchiveControl, suffix: &str) -> String {
        let id = format!("{}:{suffix}", std::process::id());
        std::fs::write(control.directory.join("archive-cycle"), &id).unwrap();
        id
    }

    #[test]
    fn acknowledged_cancel_protects_cleanup_after_power_loss() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        let id = start(&control, "power-loss");
        drop(control.request_cancel(&id).unwrap());
        std::fs::remove_file(directory.path().join("archive-cycle")).unwrap();
        std::fs::remove_file(directory.path().join(format!("archive-cycle-cancel-{id}"))).unwrap();
        assert!(control.cleanup_deferred.is_file());
    }

    #[test]
    fn failed_durable_protection_does_not_publish_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        let id = start(&control, "write-failure");
        std::fs::create_dir(&control.cleanup_deferred).unwrap();
        assert!(control.request_cancel(&id).is_err());
        assert!(!control.cycle_cancelled(&id));
    }

    #[test]
    fn repeated_cancel_is_safe_and_a_stale_dashboard_cannot_cancel_the_next_cycle() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        let first = start(&control, "first");
        drop(control.request_cancel(&first).unwrap());
        drop(control.request_cancel(&first).unwrap());
        assert!(control.cancelled());
        let next = start(&control, "next");
        assert!(!control.cancelled());
        assert_eq!(control.request_cancel(&first).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!control.cycle_cancelled(&next));
        drop(control.request_cancel(&next).unwrap());
        std::fs::write(directory.path().join(format!("archive-cycle-cancel-{first}")), b"").unwrap();
        assert!(control.cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn accepted_cancel_holds_the_same_lock_as_cleanup_and_cycle_close() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        let id = start(&control, "closing");
        let guard = control.request_cancel(&id).unwrap();
        let lock = directory.path().join("archive-cycle.lock");
        assert_eq!(crate::archive_mount_lock::acquire_path(&lock, std::time::Duration::ZERO)
            .unwrap_err().kind(), io::ErrorKind::TimedOut);
        drop(guard);
        let _close = crate::archive_mount_lock::acquire_path(&lock, std::time::Duration::ZERO).unwrap();
        assert!(control.cycle_cancelled(&id));
        assert!(control.cleanup_deferred.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn cancel_waiting_for_cleanup_rechecks_that_its_cycle_still_exists() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        let id = start(&control, "busy-cleanup");
        let lock = crate::archive_mount_lock::acquire_path(&directory.path().join("archive-cycle.lock"), std::time::Duration::ZERO).unwrap();
        let next_control = control.clone();
        let waiter = std::thread::spawn(move || next_control.request_cancel(&id));
        std::fs::remove_file(directory.path().join("archive-cycle")).unwrap();
        drop(lock);
        assert_eq!(waiter.join().unwrap().unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!control.cleanup_deferred.exists());
    }

    #[test]
    fn malformed_cycle_ids_never_become_control_paths() {
        let directory = tempfile::tempdir().unwrap();
        let control = ArchiveControl::new(directory.path());
        for id in ["", "1:", "not-a-pid:abc", "1:../outside", "1:a/b", "1:a:b"] {
            std::fs::write(directory.path().join("archive-cycle"), id).unwrap();
            assert!(control.active_cycle().is_none(), "accepted malformed ID: {id:?}");
        }
    }
}
