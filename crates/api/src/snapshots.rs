//! Snapshot management API.
//!
//! XFS reflink snapshots live at
//! `/backingfiles/snapshots/snap-<id>/snap.bin`. This module provides:
//!
//!   * `GET    /api/snapshots`               — list with size/timestamp
//!   * `DELETE /api/snapshots/:id`           — delete one snapshot
//!   * `GET    /api/backingfiles/free-space` — total/used/avail in bytes
//!
//! Deletion uses `/root/bin/release_snapshot.sh` when available so it shares
//! the runtime's unmount and symlink-cleanup behavior.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::router::AppState;

const SNAPSHOTS_DIR: &str = "/backingfiles/snapshots";
const RELEASE_SNAPSHOT_SCRIPT: &str = "/root/bin/release_snapshot.sh";
/// The live image retains shared extents independently of snapshots.
const CAM_DISK: &str = "/backingfiles/cam_disk.bin";

/// One snapshot entry in the listing response.
#[derive(serde::Serialize)]
struct SnapshotEntry {
    /// `snap-<id>` directory name. Used as the path parameter for delete.
    id: String,
    /// Estimated bytes freed by deleting THIS snapshot **and every older
    /// one**, not this snapshot alone.
    ///
    /// Reflinked blocks are freed only when their final holder is deleted, so
    /// reclaim is meaningful for an oldest-first prefix rather than one file.
    /// The final row should equal the independently derived total footprint.
    ///
    /// `None` means "not measured yet, or could not be measured" — render
    /// as pending/unavailable, NEVER as `0 B`. A measured zero is
    /// legitimate (a run that frees nothing) and does render as `0 B`.
    cumulative_reclaim_bytes: Option<u64>,
    /// Number of older snapshots in the reclaim prefix.
    older_count: usize,
    /// Unix epoch seconds from `snap.bin` mtime.
    created_unix: i64,
}

/// How long a measurement stays usable before we re-measure.
const SIZE_MAX_AGE_SECS: u64 = 15 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn size_cache() -> &'static std::sync::Mutex<sentryusb_gadget::reflink::SizeCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<sentryusb_gadget::reflink::SizeCache>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(sentryusb_gadget::reflink::SizeCache::new()))
}

static REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Clears the in-flight flag even if measurement panics.
struct RefreshGuard;
impl Drop for RefreshGuard {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Measure every snapshot off the request path.
///
/// Extent-map walks run off the request path, one refresh at a time.
/// `ids` must be OLDEST-FIRST: the cumulative figure for a snapshot is
/// "delete this and everything older", so the prefix order is the meaning.
fn spawn_size_refresh(ids: Vec<String>, generation: u64) {
    use std::sync::atomic::Ordering;
    if REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let _guard = RefreshGuard;

        // One missing extent map invalidates every later cumulative value.
        let mut maps: Vec<Vec<sentryusb_gadget::reflink::PhysicalRange>> =
            Vec::with_capacity(ids.len());
        let mut idents = Vec::with_capacity(ids.len());
        let mut failed: Option<(String, String)> = None;
        for id in &ids {
            let bin = format!("{}/{}/snap.bin", SNAPSHOTS_DIR, id);
            let p = std::path::Path::new(&bin);
            match (|| -> anyhow::Result<_> {
                let before = sentryusb_gadget::reflink::file_identity(p)?;
                let map = sentryusb_gadget::reflink::extent_map(p)?;
                let after = sentryusb_gadget::reflink::file_identity(p)?;
                anyhow::ensure!(before == after, "Snapshot changed during measurement");
                Ok((map, after))
            })()
            {
                Ok((m, ident)) => {
                    maps.push(m);
                    idents.push(ident);
                }
                Err(e) => {
                    failed = Some((id.clone(), e.to_string()));
                    break;
                }
            }
        }

        // Extents retained by the live image are not reclaimable.
        let external = if failed.is_none() {
            match sentryusb_gadget::reflink::extent_map(std::path::Path::new(CAM_DISK)) {
                Ok(m) => Some(m),
                Err(e) => {
                    failed = Some((CAM_DISK.to_string(), e.to_string()));
                    None
                }
            }
        } else {
            None
        };

        let entries = match (failed, external) {
            (None, Some(external)) => {
                let curve = sentryusb_gadget::reflink::cumulative_reclaim(&maps, &external);
                ids.iter()
                    .zip(curve)
                    .zip(idents)
                    .map(|((id, bytes), ident)| (id.clone(), (bytes, ident)))
                    .collect()
            }
            (failed, _) => {
                if let Some((id, e)) = failed {
                    tracing::warn!("cumulative snapshot sizing failed at {}: {}", id, e);
                }
                Vec::new()
            }
        };

        if let Ok(mut cache) = size_cache().lock() {
            // Do not republish values invalidated by a concurrent deletion.
            if !cache.publish(generation, ids, entries, now_secs()) {
                tracing::debug!("discarded a snapshot-size measurement invalidated mid-flight");
            }
        }
    });
}

/// GET /api/snapshots
///
/// Returns the list of snapshot directories under `/backingfiles/snapshots/`.
/// Sorted oldest-first so callers can default to that ordering — the
/// user typically wants to delete the oldest to free space.
pub async fn list_snapshots(
    State(_s): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut entries: Vec<SnapshotEntry> = Vec::new();

    let dir = match std::fs::read_dir(SNAPSHOTS_DIR) {
        Ok(d) => d,
        Err(_) => {
            return (StatusCode::OK, Json(serde_json::json!({
                "snapshots": entries,
            })));
        }
    };

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_snapshot_id(&name) {
            continue;
        }
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }

        // Use `snap.bin` mtime because autofs metadata updates the directory
        // mtime when an old snapshot is viewed.
        let created_unix = std::fs::symlink_metadata(path.join("snap.bin"))
            .or_else(|_| entry.metadata())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        entries.push(SnapshotEntry {
            id: name,
            // Filled from the extent-map cache below.
            cumulative_reclaim_bytes: None,
            older_count: 0,
            created_unix,
        });
    }

    // Cumulative reclaim values are defined against oldest-first order.
    entries.sort_by(|a, b| a.created_unix.cmp(&b.created_unix).then_with(|| a.id.cmp(&b.id)));
    for (i, e) in entries.iter_mut().enumerate() {
        e.older_count = i;
    }

    // A stale value may describe a different snapshot set, so omit it and refresh.
    let now = now_secs();
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let (current, computed_at, generation) = match size_cache().lock() {
        Ok(mut cache) => {
            let mut current = cache.is_current_for(&ids, now, SIZE_MAX_AGE_SECS);
            if current && cache.has_measurements() {
                let values: Option<Vec<u64>> = entries.iter().map(|entry| {
                    let bin = format!("{}/{}/snap.bin", SNAPSHOTS_DIR, entry.id);
                    sentryusb_gadget::reflink::file_identity(std::path::Path::new(&bin)).ok()
                        .and_then(|identity| cache.get_if_same(&entry.id, identity))
                }).collect();
                if let Some(values) = values {
                    for (entry, bytes) in entries.iter_mut().zip(values) {
                        entry.cumulative_reclaim_bytes = Some(bytes);
                    }
                } else {
                    // Every prefix depends on every holder. A reused/missing
                    // inode invalidates the entire curve, not just its own row.
                    cache.invalidate();
                    current = false;
                }
            }
            (current, cache.computed_at(), cache.generation())
        }
        Err(_) => (false, None, 0),
    };
    if !current && !ids.is_empty() {
        spawn_size_refresh(ids, generation);
    }
    // Derive pending state from the cache to avoid racing worker publication.
    // Failed measurements still count as completed attempts.
    let pending = !current && !entries.is_empty();

    // `du` double-counts reflinked files. Derive the aggregate exclusive
    // footprint as filesystem-used bytes minus non-snapshot file usage.
    let total_allocated_bytes = if entries.is_empty() {
        Some(0)
    } else {
        async {
            let df = sentryusb_shell::run("df", &["--output=used", "--block-size=1", "/backingfiles/"]).await.ok()?;
            let used = df.lines().last()?.split_whitespace().next()?.parse::<u64>().ok()?;
            let du = sentryusb_shell::run("du", &["-sB1", "--exclude=snapshots", "/backingfiles/"]).await.ok()?;
            let other = du.split_whitespace().next()?.parse::<u64>().ok()?;
            Some(used.saturating_sub(other))
        }.await
    };

    if let (Some(total), Some(last)) = (total_allocated_bytes, entries.last().and_then(|entry| entry.cumulative_reclaim_bytes)) {
        let high = total.max(last);
        let low = total.min(last);
        // Independent check allows metadata and live-write noise.
        if high > 0 && u128::from(high - low) * 10 > u128::from(high) * 3 {
            tracing::warn!("Snapshot accounting disagreement: extent estimate={last}, filesystem estimate={total}");
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "snapshots": entries,
        "total_allocated_bytes": total_allocated_bytes,
        // Distinguish an active measurement from unavailable data.
        "sizes_computed_at": computed_at,
        "sizes_pending": pending,
    })))
}

fn valid_snapshot_id(id: &str) -> bool {
    id.strip_prefix("snap-")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
}

/// DELETE /api/snapshots/:id
///
/// Calls `release_snapshot.sh` to umount the snap.bin loop image and
/// remove the directory + dangling /mutable/Recordings symlinks. The
/// id must be a `snap-*` name; reject anything else to prevent
/// arbitrary path traversal.
pub async fn delete_snapshot(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !valid_snapshot_id(&id) {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            "Invalid snapshot id (expected snap-<digits>)",
        );
    }

    let path = format!("{}/{}", SNAPSHOTS_DIR, id);
    if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
        return crate::json_error(StatusCode::NOT_FOUND, "Snapshot not found");
    }

    // The shared runtime serializes unmount and recording-link cleanup. Never
    // fall back to raw rm: that bypasses mount checks and leaves index links.
    let script_exists = std::path::Path::new(RELEASE_SNAPSHOT_SCRIPT).exists();
    let result = if script_exists {
        sentryusb_shell::run(RELEASE_SNAPSHOT_SCRIPT, &[id.as_str()]).await
    } else {
        return crate::json_error(StatusCode::SERVICE_UNAVAILABLE,
            "Snapshot cleanup helper is unavailable. Repair the installation before deleting snapshots.");
    };

    match result {
        Ok(_) => {
            // Deleting one holder changes every cumulative reclaim value.
            if let Ok(mut cache) = size_cache().lock() {
                cache.invalidate();
            }
            (StatusCode::OK, Json(serde_json::json!({"deleted": id})))
        }
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to delete snapshot: {}", e),
        ),
    }
}

/// Total, used and available bytes for the backingfiles partition. Feeds the
/// snapshot UI's space gauge and the wizard pre-flight's size-rejection error.
pub async fn get_free_space(
    State(_s): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    match tokio::task::spawn_blocking(crate::status::managed_storage_health).await {
        Ok(health) => (StatusCode::OK, Json(serde_json::json!({
            "total_bytes": health.total_bytes,
            "used_bytes": health.total_bytes.saturating_sub(health.free_bytes),
            "available_bytes": health.free_bytes,
            "mounted": health.total_bytes > 0,
            "storage_health": health,
        }))),
        Err(_) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Storage status unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::valid_snapshot_id;

    #[test]
    fn snapshot_ids_require_an_entire_numeric_slot() {
        for id in ["snap-0", "snap-000123"] { assert!(valid_snapshot_id(id)); }
        for id in ["snap-", "snap-one", "snap-../other", "snap-1/other", "/snap-1", "snap-1\n", "snap-١"] {
            assert!(!valid_snapshot_id(id), "accepted {id:?}");
        }
    }
}
