//! Cross-process ownership of the `/mnt/archive` network mount.
//!
//! Two parties mount the CIFS/NFS archive share: archiveloop's
//! connect-archive.sh at the start of each archive cycle, and the backup path
//! in `backup.rs` when the user hits Backup Now with no cycle running. Both
//! MUST hold an exclusive `flock` on [`ARCHIVE_MOUNT_LOCK_PATH`] across their
//! mount/unmount transitions (archiveloop via `flock` on fd 210 in
//! connect/disconnect-archive.sh; keep the path in sync with those scripts).
//! Without it, archiveloop can adopt a mount the backup created and then have
//! it unmounted mid-cycle, or its disconnect can `umount -f -l` a backup
//! mid-write.
//!
//! Deliberately NOT held across a whole archive cycle: post-archive-process.sh
//! curls the backup API while its cycle runs, so a cycle-scoped lock would
//! deadlock that in-flight request (the same trap the gadget cycle_lock doc
//! comment warns about). The lock covers only mount, use and unmount windows;
//! a long rsync runs lock-free on a mount archiveloop owns.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// Must match `ARCHIVE_MOUNT_LOCK` in run/{cifs,nfs}_archive/
/// connect-archive.sh and disconnect-archive.sh.
pub const ARCHIVE_MOUNT_LOCK_PATH: &str = "/tmp/sentryusb_archive_mount.lock";

/// Exclusive hold on the archive-mount lock, released on drop (the flock dies
/// with the file handle).
#[derive(Debug)]
pub struct ArchiveMountGuard {
    _file: File,
}

/// Acquire the archive-mount flock, waiting up to `timeout` for whoever holds
/// it (archiveloop holds it for seconds around a mount/unmount). Polls
/// `LOCK_NB` rather than parking in `flock(2)` so the wait stays bounded.
/// Blocking: call it from a blocking thread.
pub fn acquire(timeout: Duration) -> io::Result<ArchiveMountGuard> {
    acquire_path(Path::new(ARCHIVE_MOUNT_LOCK_PATH), timeout)
}

fn acquire_path(path: &Path, timeout: Duration) -> io::Result<ArchiveMountGuard> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false) // content is irrelevant; the flock is the point
        .open(path)?;
    let deadline = Instant::now() + timeout;
    loop {
        if try_flock_exclusive(&file)? {
            return Ok(ArchiveMountGuard { _file: file });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "archive mount lock held elsewhere for over {}s (archive connect/disconnect in progress)",
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
    // Same primitive as shell `flock`: the lock lives on the open file
    // description, so it also excludes other threads of this process and
    // stays held across await points until the guard drops.
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
        let path = dir.path().join("archive.lock");
        let g1 = acquire_path(&path, Duration::from_millis(0)).unwrap();
        let err = acquire_path(&path, Duration::from_millis(300)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        drop(g1);
        let _g2 = acquire_path(&path, Duration::from_millis(0)).unwrap();
    }
}
