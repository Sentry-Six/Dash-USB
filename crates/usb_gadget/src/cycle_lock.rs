//! Cross-process gadget-cycle lock shared with archiveloop. Hold it around an
//! entire disable/work/enable sequence to prevent simultaneous block writers.
//! Do not acquire inside enable/disable because locked shell shims call them.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// Must match `GADGET_CYCLE_LOCK` in `run/archiveloop`.
pub const GADGET_CYCLE_LOCK_PATH: &str = "/tmp/sentryusb_gadget_cycle.lock";

/// Exclusive hold on the gadget-cycle lock; released on drop (the flock
/// dies with the file handle).
#[derive(Debug)]
pub struct CycleGuard {
    _file: File,
}

/// Acquire the flock with a bounded poll; call from a blocking thread.
pub fn acquire(timeout: Duration) -> io::Result<CycleGuard> {
    acquire_path(Path::new(GADGET_CYCLE_LOCK_PATH), timeout)
}

pub(crate) fn acquire_path(path: &Path, timeout: Duration) -> io::Result<CycleGuard> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    let deadline = Instant::now() + timeout;
    loop {
        if try_flock_exclusive(&file)? {
            return Ok(CycleGuard { _file: file });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "gadget cycle lock held elsewhere for over {}s (archive sync or stall recovery in progress)",
                    timeout.as_secs()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(unix)]
fn try_flock_exclusive(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    // The open file description holds the same lock used by shell flock.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EWOULDBLOCK) | Some(libc::EINTR) => Ok(false),
        _ => Err(err),
    }
}

#[cfg(not(unix))]
fn try_flock_exclusive(_file: &File) -> io::Result<bool> {
    Ok(true) // no archiveloop to race on non-unix dev hosts
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_times_out_while_held_then_succeeds_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cycle.lock");
        let g1 = acquire_path(&path, Duration::from_millis(0)).unwrap();
        let err = acquire_path(&path, Duration::from_millis(300)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        drop(g1);
        let _g2 = acquire_path(&path, Duration::from_millis(0)).unwrap();
    }

    #[test]
    fn acquire_waits_for_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cycle.lock");
        let g1 = acquire_path(&path, Duration::from_millis(0)).unwrap();
        let p2 = path.clone();
        let waiter =
            std::thread::spawn(move || acquire_path(&p2, Duration::from_secs(10)).is_ok());
        std::thread::sleep(Duration::from_millis(400));
        drop(g1);
        assert!(waiter.join().unwrap());
    }
}
