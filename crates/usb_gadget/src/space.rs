//! Retain snapshots within both backing-storage and recording-index capacity.

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

const BACKINGFILES: &str = "/backingfiles";
const MUTABLE: &str = "/mutable";
const INODE_STALL_LATCH: &str = "/run/dashusb_inode_stall";

/// Match archiveloop: 10 GiB plus approximately 3% of the backing filesystem.
fn default_reserve(total: u64) -> u64 {
    10_737_418_240 + total / 33
}

/// Keep inode headroom for /mutable/Recordings links and state-file writes.
/// The quarter-table cap is essential on small single-card installations.
pub fn inode_reserve(total_inodes: u64) -> u64 {
    (total_inodes / 20).max(20_000).min(total_inodes / 4)
}

fn mutable_rw_in(mounts: &str) -> bool {
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        if fields.next() != Some(MUTABLE) { return false; }
        let _filesystem = fields.next();
        fields.next().is_some_and(|options| options.split(',').any(|option| option == "rw"))
    })
}

fn mutable_rw_mounted() -> bool {
    std::fs::read_to_string("/proc/mounts").is_ok_and(|mounts| mutable_rw_in(&mounts))
}

#[derive(Clone, Copy, Debug)]
struct Inodes { total: u64, free: u64 }

/// Unknown or read-only mount state must never trigger destructive cleanup.
fn inode_pressure() -> Option<Inodes> {
    if !mutable_rw_mounted() { return None; }
    let stats = get_inodes(MUTABLE).ok()?;
    if inode_reserve(stats.total) >= stats.total { return None; }
    Some(stats)
}

fn inode_target_met(stats: Option<Inodes>) -> bool {
    stats.is_none_or(|stats| stats.free > inode_reserve(stats.total))
}

#[derive(Default)]
struct StallGuard { releases_without_gain: usize }
impl StallGuard {
    fn record(&mut self, block_target_met: bool, before: Option<Inodes>, after: Option<Inodes>) -> bool {
        if block_target_met && matches!((before, after), (Some(before), Some(after)) if after.free <= before.free) {
            self.releases_without_gain += 1;
        } else {
            self.releases_without_gain = 0;
        }
        self.releases_without_gain >= 3
    }
}

fn halt_inode_eviction(stats: Option<Inodes>) -> Result<()> {
    if let Some(stats) = stats {
        // /mutable may itself be full. A tmpfs latch survives retries, not boots.
        std::fs::write(INODE_STALL_LATCH, stats.free.to_string())
            .context("could not persist inode-cleanup stop marker")?;
    }
    bail!("Recording index has too few free inodes and snapshot cleanup cannot relieve it. Inspect /mutable before retrying cleanup.")
}

#[derive(Clone, Debug)]
struct Snapshot {
    name: String,
    modified: std::time::SystemTime,
    completed: bool,
}

/// Preserve the newest completed snapshot by both date and numeric slot. A
/// clock rollback must not cause the newest recording snapshot to be evicted.
fn release_candidates(mut snapshots: Vec<Snapshot>) -> Vec<String> {
    let newest = snapshots.iter().filter(|snapshot| snapshot.completed)
        .max_by_key(|snapshot| (snapshot.modified, &snapshot.name)).map(|snapshot| snapshot.name.clone());
    let highest = snapshots.iter().filter(|snapshot| snapshot.completed)
        .filter_map(|snapshot| snapshot.name.strip_prefix("snap-")?.parse::<u64>().ok().map(|slot| (slot, snapshot.name.clone())))
        .max().map(|(_, name)| name);
    snapshots.sort_by_key(|snapshot| (snapshot.modified, snapshot.name.clone()));
    snapshots.into_iter().filter(|snapshot| Some(&snapshot.name) != newest.as_ref() && Some(&snapshot.name) != highest.as_ref())
        .map(|snapshot| snapshot.name).collect()
}

fn snapshots_to_release() -> Vec<String> {
    release_candidates(super::snapshot::list_snapshots().into_iter().map(|name| {
        let path = std::path::Path::new(BACKINGFILES).join("snapshots").join(&name);
        Snapshot {
            name,
            modified: std::fs::symlink_metadata(path.join("snap.bin")).and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH),
            completed: path.join("snap.bin").is_file() && path.join("snap.bin.toc").is_file(),
        }
    }).collect())
}

/// Called every housekeeping cycle, even when free bytes are plentiful: the
/// clip index can run out of inodes long before a large backing disk fills.
pub async fn manage_free_space(reserve_bytes: Option<u64>) -> Result<()> {
    let _lock = super::snapshot::acquire_mgmt_lock()?;
    let (total, mut free) = get_space(BACKINGFILES)?;
    let reserve = reserve_bytes.unwrap_or_else(|| default_reserve(total));
    if reserve >= total {
        bail!("Reserve {reserve} >= filesystem size {total}; refusing to delete snapshots toward an impossible target");
    }

    let mut inodes = inode_pressure();
    let mut inode_suspended = false;
    if let Ok(latch) = std::fs::read_to_string(INODE_STALL_LATCH) {
        match (latch.trim().parse::<u64>().ok(), inodes) {
            (Some(latched), Some(now)) if now.free > latched => {
                std::fs::remove_file(INODE_STALL_LATCH).context("clear recovered inode-cleanup marker")?;
            }
            _ => inode_suspended = true,
        }
    }
    if inode_suspended { inodes = None; }
    if free >= reserve && inode_target_met(inodes) { return Ok(()); }

    info!("Releasing old snapshots: {free}/{total} bytes free, reserve={reserve}; index inodes={inodes:?}");
    let candidates = snapshots_to_release();
    let mut stall = StallGuard::default();
    for snapshot in candidates {
        // Recheck before every deletion. The filesystem may remount read-only
        // after an I/O error while this loop is running.
        inodes = if inode_suspended { None } else { inode_pressure() };
        if free >= reserve && inode_target_met(inodes) { return Ok(()); }
        if let Err(error) = super::snapshot::release_snapshot_unlocked(&snapshot).await {
            warn!("Could not release {snapshot}: {error:#}");
            continue;
        }
        let (_, new_free) = get_space(BACKINGFILES)?;
        let new_inodes = if inode_suspended { None } else { inode_pressure() };
        info!("Released {snapshot}: {new_free} bytes free; index inodes={new_inodes:?}");
        free = new_free;
        if free >= reserve && inode_target_met(new_inodes) { return Ok(()); }
        if stall.record(free >= reserve, inodes, new_inodes) { return halt_inode_eviction(new_inodes); }
        inodes = new_inodes;
    }
    if free >= reserve && !inode_target_met(inodes) { return halt_inode_eviction(inodes); }
    if free < reserve {
        bail!("Storage remains below its {reserve}-byte reserve; the newest completed snapshots were preserved");
    }
    Ok(())
}

fn read_stat(path: &str, format: &str, expected: usize) -> Result<Vec<u64>> {
    let output = std::process::Command::new("stat").args(["--file-system", format, path]).output()?;
    if !output.status.success() { bail!("stat failed for {path}"); }
    let fields = String::from_utf8_lossy(&output.stdout).split_whitespace().map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if fields.len() != expected { bail!("unexpected stat output for {path}"); }
    Ok(fields)
}

fn get_space(path: &str) -> Result<(u64, u64)> {
    let fields = read_stat(path, "--format=%b %S %f", 3)?;
    let (blocks, block_size, free_blocks) = (fields[0], fields[1], fields[2]);
    if blocks == 0 || block_size == 0 || free_blocks > blocks { bail!("invalid filesystem capacity for {path}"); }
    Ok((blocks.checked_mul(block_size).context("filesystem capacity overflow")?,
        free_blocks.checked_mul(block_size).context("free space overflow")?))
}

fn get_inodes(path: &str) -> Result<Inodes> {
    let fields = read_stat(path, "--format=%c %d", 2)?;
    if fields[0] == 0 || fields[1] > fields[0] { bail!("invalid inode capacity for {path}"); }
    Ok(Inodes { total: fields[0], free: fields[1] })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inodes(free: u64) -> Option<Inodes> { Some(Inodes { total: 120_960, free }) }
    fn snapshot(slot: u64, time: u64, completed: bool) -> Snapshot {
        Snapshot { name: format!("snap-{slot:06}"), modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(time), completed }
    }

    #[test]
    fn reserve_matches_archiveloop_and_stays_reachable_on_small_inode_tables() {
        assert_eq!(default_reserve(330_000_000_000), 10_737_418_240 + 10_000_000_000);
        for total in [1, 11_856, 19_200, 24_000, 120_960, 472_000] {
            assert!(inode_reserve(total) < total);
            assert!(inode_reserve(total) <= total / 4);
        }
        assert_eq!(inode_reserve(11_856), 2_964);
        assert_eq!(inode_reserve(120_960), 20_000);
        assert_eq!(inode_reserve(472_000), 23_600);
    }

    #[test]
    fn mount_safety_requires_the_actual_mutable_read_write_mount() {
        assert!(!mutable_rw_in("/dev/root / ext4 rw 0 0\n"));
        assert!(!mutable_rw_in("/dev/sda2 /mutable ext4 ro,relatime 0 0\n"));
        assert!(!mutable_rw_in("/dev/sda2 /mutable-other ext4 rw 0 0\n"));
        assert!(mutable_rw_in("/dev/sda2 /mutable ext4 rw,relatime 0 0\n"));
    }

    #[test]
    fn unknown_inode_data_disables_inode_cleanup_and_low_inodes_trigger_it() {
        assert!(inode_target_met(None));
        assert!(!inode_target_met(inodes(20_000)));
        assert!(inode_target_met(inodes(20_001)));
    }

    #[test]
    fn three_releases_without_inode_gain_stop_further_deletion() {
        let mut guard = StallGuard::default();
        assert!(!guard.record(true, inodes(100), inodes(100)));
        assert!(!guard.record(true, inodes(100), inodes(99)));
        assert!(guard.record(true, inodes(99), inodes(99)));
    }

    #[test]
    fn progress_block_pressure_and_unknown_stats_reset_stall_counter() {
        let mut guard = StallGuard::default();
        for reset in [(true, inodes(100), inodes(200)), (false, inodes(100), inodes(100)), (true, inodes(100), None)] {
            assert!(!guard.record(true, inodes(100), inodes(100)));
            assert!(!guard.record(true, inodes(100), inodes(100)));
            assert!(!guard.record(reset.0, reset.1, reset.2));
        }
    }

    #[test]
    fn clock_rollback_preserves_both_newest_completed_snapshots() {
        let candidates = release_candidates(vec![snapshot(42, 1, true), snapshot(10, 2, true), snapshot(11, 3, true), snapshot(12, 4, true)]);
        assert_eq!(candidates, ["snap-000010", "snap-000011"]);
    }

    #[test]
    fn incomplete_snapshot_does_not_displace_protected_completed_snapshot() {
        assert_eq!(release_candidates(vec![snapshot(1, 1, true), snapshot(2, 2, false)]), ["snap-000002"]);
        assert!(release_candidates(vec![snapshot(1, 1, true)]).is_empty());
        assert!(release_candidates(vec![]).is_empty());
    }
}
