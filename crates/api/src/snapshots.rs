//! Snapshot management API.
//!
//! Snapshots are XFS reflink-backed point-in-time copies of cam_disk that
//! archiveloop takes on a schedule (default every 58 minutes), living at
//! `/backingfiles/snapshots/snap-<id>/snap.bin`. The runtime's
//! `manage_free_space.sh` prunes them automatically; these endpoints are the
//! user's explicit route to inspect and reclaim that space.
//!
//! Deletes shell out to `/root/bin/release_snapshot.sh`, which the free-space
//! manager also uses, rather than reimplementing its umount and symlink cleanup.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::router::AppState;

const SNAPSHOTS_DIR: &str = "/backingfiles/snapshots";
const RELEASE_SNAPSHOT_SCRIPT: &str = "/root/bin/release_snapshot.sh";

#[derive(serde::Serialize)]
struct SnapshotEntry {
    /// `snap-<id>` directory name, also the path parameter for delete.
    id: String,
    /// Apparent size from `du`. Not reflink-aware, so an upper bound only.
    size_bytes: u64,
    /// Directory mtime in Unix epoch seconds. The UI sorts and formats on it.
    created_unix: i64,
}

/// Snapshot directories under `/backingfiles/snapshots/`, oldest first: the
/// user is normally deleting the oldest to free space.
pub async fn list_snapshots(
    State(_s): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut entries: Vec<SnapshotEntry> = Vec::new();

    let dir = match std::fs::read_dir(SNAPSHOTS_DIR) {
        Ok(d) => d,
        Err(_) => {
            // A missing directory just means no snapshots have been taken.
            return (StatusCode::OK, Json(serde_json::json!({
                "snapshots": entries,
            })));
        }
    };

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("snap-") {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // mtime stands in for creation time. manage_free_space.sh instead
        // sorts by alphabetic snap-<id>, close enough to agree for the UI.
        let created_unix = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Apparent allocated bytes (st_blocks * 512), NOT reflink-aware: each
        // snap.bin is a `cp --reflink=always` of cam_disk.bin, so its st_blocks
        // reports the full cam_disk block count even though those extents are
        // shared with the live image and the other snapshots. Upper bound only,
        // not "what deleting this one snapshot reclaims". The aggregate
        // `total_allocated_bytes` below recovers the true exclusive footprint.
        let du_out = sentryusb_shell::run(
            "du", &["-sB1", &path.to_string_lossy()],
        ).await.unwrap_or_default();
        let size_bytes: u64 = du_out
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        entries.push(SnapshotEntry {
            id: name,
            size_bytes,
            created_unix,
        });
    }

    entries.sort_by_key(|e| e.created_unix);

    // Bytes that deleting every snapshot would free. `du` cannot answer this:
    // it dedupes hard links by inode, but each snap.bin is a separate inode
    // sharing extents with cam_disk.bin, so summing per-file st_blocks yields
    // N * cam_disk_size, far larger than the partition itself.
    //
    // The reflink-exclusive footprint is instead:
    //     df_used(/backingfiles) - du(--exclude=snapshots /backingfiles/)
    // Partition-level used bytes count each allocated extent once however many
    // files reference it. Deleting all snapshots leaves only the non-snapshot
    // files (chiefly cam_disk.bin), whose blocks XFS retains, so `df` settles
    // to that du value and the difference is what the snapshots hold alone.
    let total_allocated_bytes: u64 = if entries.is_empty() {
        0
    } else {
        let df_out = sentryusb_shell::run(
            "df", &["--output=used", "--block-size=1", "/backingfiles/"],
        ).await.unwrap_or_default();
        let used_bytes: u64 = df_out
            .lines()
            .last()
            .and_then(|l| l.split_whitespace().next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let non_snap_out = sentryusb_shell::run(
            "du", &["-sB1", "--exclude=snapshots", "/backingfiles/"],
        ).await.unwrap_or_default();
        let non_snap_bytes: u64 = non_snap_out
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        used_bytes.saturating_sub(non_snap_bytes)
    };

    (StatusCode::OK, Json(serde_json::json!({
        "snapshots": entries,
        "total_allocated_bytes": total_allocated_bytes,
    })))
}

/// Calls `release_snapshot.sh` to umount the snap.bin loop image and remove the
/// directory along with dangling /mutable/Recordings symlinks. The id MUST be a
/// `snap-*` name with no separators; anything else is path traversal.
pub async fn delete_snapshot(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !id.starts_with("snap-") || id.contains('/') || id.contains("..") {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            "Invalid snapshot id (expected snap-<digits>)",
        );
    }

    let path = format!("{}/{}", SNAPSHOTS_DIR, id);
    if !std::path::Path::new(&path).is_dir() {
        return crate::json_error(StatusCode::NOT_FOUND, "Snapshot not found");
    }

    // Prefer the on-disk script to share the runtime's umount and symlink
    // cleanup. Plain rm is a fallback for partially-installed systems only.
    //
    // Pass the bare `id`, NOT the full path: `release_snapshot.sh` is a thin
    // shim forwarding "$@" to `dashusb snapshot release`, which expects a
    // `snap-NNNNNN` name. It also accepts a full path, so the bare id works
    // against both the shim and full-script installs.
    let script_exists = std::path::Path::new(RELEASE_SNAPSHOT_SCRIPT).exists();
    let result = if script_exists {
        sentryusb_shell::run(RELEASE_SNAPSHOT_SCRIPT, &[id.as_str()]).await
    } else {
        sentryusb_shell::run("rm", &["-rf", &path]).await
    };

    match result {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"deleted": id}))),
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
    let df = sentryusb_shell::run(
        "df", &["--output=size,used,avail", "--block-size=1", "/backingfiles/"],
    ).await;

    let (total, used, avail) = match df {
        Ok(out) => {
            let line = out.lines().last().unwrap_or("");
            let mut it = line.split_whitespace();
            let total: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let used: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let avail: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            (total, used, avail)
        }
        Err(_) => (0, 0, 0),
    };

    (StatusCode::OK, Json(serde_json::json!({
        "total_bytes": total,
        "used_bytes": used,
        "available_bytes": avail,
        "mounted": total > 0,
    })))
}
