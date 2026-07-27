//! Free space management for the backing filesystem: release old snapshots
//! when space runs low.
//!
//! `run/archiveloop`'s freespacemanager computes the reserve (10 GB + 3% of
//! the filesystem) and passes it through `manage_free_space.sh`. Manual CLI
//! use passes none, so the same formula is duplicated in [`default_reserve`]
//! and must stay in step with archiveloop.

use anyhow::Result;
use tracing::{info, warn};

const BACKINGFILES: &str = "/backingfiles";

/// 10 GB + 3% of the filesystem. Must match `freespacemanager` in
/// `run/archiveloop`.
fn default_reserve(total: u64) -> u64 {
    10_737_418_240 + total / 33
}

/// Release old snapshots while free space is below the reserve.
/// `reserve_bytes` comes from the CLI (archiveloop passes its computed
/// reserve); `None` falls back to [`default_reserve`].
pub async fn manage_free_space(reserve_bytes: Option<u64>) -> Result<()> {
    let _lock = super::snapshot::acquire_mgmt_lock()?;

    let (total, free) = get_space(BACKINGFILES)?;
    if total == 0 {
        return Ok(());
    }
    let reserve = reserve_bytes.unwrap_or_else(|| default_reserve(total));

    info!(
        "Disk space: {} free / {} total (reserve {})",
        free, total, reserve
    );
    if free >= reserve {
        return Ok(());
    }

    info!("Free space below reserve, releasing old snapshots...");

    let snapshots = super::snapshot::list_snapshots();
    if snapshots.is_empty() {
        warn!("No snapshots to release, disk is full");
        return Ok(());
    }

    // Oldest first, but NEVER the newest COMPLETED snapshot: it holds the
    // only captured copy of the most recent footage (the live drive rolls
    // over), and make_snapshot's TOC diff needs a predecessor. Completed
    // means both snap.bin and its committed .toc exist; a directory left by
    // a crash mid-make protects nothing and stays releasable even when it
    // sorts newest.
    let keep = snapshots.iter().rev().find(|s| snapshot_is_completed(s));
    let releasable: Vec<&String> = snapshots
        .iter()
        .filter(|s| Some(*s) != keep)
        .collect();
    if releasable.is_empty() {
        warn!(
            "Nothing releasable ({:?} is the only completed snapshot) — low space persists",
            keep
        );
        return Ok(());
    }

    let mut free_now = free;
    for snap in releasable {
        // The mgmt lock is already held here, so use the unlocked release.
        if let Err(e) = super::snapshot::release_snapshot_unlocked(snap).await {
            warn!("Failed to release {}: {}", snap, e);
            continue;
        }

        let (_, new_free) = get_space(BACKINGFILES)?;
        info!("After releasing {}: {} bytes free", snap, new_free);
        free_now = new_free;
        if new_free >= reserve {
            break;
        }
    }

    if free_now < reserve {
        warn!(
            "Released all eligible snapshots but free space ({}) is still below reserve ({}) — \
             the SD card may be too small for the configured drive size",
            free_now, reserve
        );
    }

    Ok(())
}

/// Completed means both the disk image and the committed TOC exist.
/// `make_snapshot` renames `snap.bin.toc_` to `snap.bin.toc` as its final
/// step, so a missing TOC means the snapshot was abandoned.
fn snapshot_is_completed(snap_name: &str) -> bool {
    let dir = format!("{}/snapshots/{}", BACKINGFILES, snap_name);
    std::path::Path::new(&format!("{}/snap.bin", dir)).exists()
        && std::path::Path::new(&format!("{}/snap.bin.toc", dir)).exists()
}

/// Returns `(total_bytes, free_bytes)`.
fn get_space(path: &str) -> Result<(u64, u64)> {
    let output = std::process::Command::new("stat")
        .args(["--file-system", "--format=%b %S %f", path])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("stat failed for {}", path);
    }

    let s = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = s.trim().split_whitespace().collect();
    if parts.len() >= 3 {
        let blocks: u64 = parts[0].parse().unwrap_or(0);
        let block_size: u64 = parts[1].parse().unwrap_or(0);
        let free_blocks: u64 = parts[2].parse().unwrap_or(0);
        return Ok((blocks * block_size, free_blocks * block_size));
    }

    Ok((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_reserve_matches_bash_formula() {
        assert_eq!(default_reserve(0), 10_737_418_240);
        assert_eq!(default_reserve(330_000_000_000), 10_737_418_240 + 10_000_000_000);
    }
}
