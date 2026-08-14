//! Release old snapshots below the reserve shared with archiveloop.

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
    // Reject impossible reserves instead of deleting every snapshot to chase one.
    if reserve >= total {
        warn!(
            "Reserve {} >= filesystem size {} — refusing to evict; no amount of \
             deletion can satisfy it (check the reserve, or the disk is too small)",
            reserve, total
        );
        return Ok(());
    }

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

    // Preserve the newest completed snapshot for recent footage and TOC diffing.
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
    // Fail closed: malformed capacity data must never look like zero free space.
    if parts.len() < 3 {
        anyhow::bail!("unexpected stat output for {}: {:?}", path, s.trim());
    }
    let field = |i: usize, what: &str| -> Result<u64> {
        parts[i]
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("stat {} for {}: {:?} ({})", what, path, parts[i], e))
    };
    let blocks = field(0, "total blocks")?;
    let block_size = field(1, "block size")?;
    let free_blocks = field(2, "free blocks")?;
    if block_size == 0 {
        anyhow::bail!("stat reported a zero block size for {}", path);
    }
    Ok((
        blocks.saturating_mul(block_size),
        free_blocks.saturating_mul(block_size),
    ))
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
