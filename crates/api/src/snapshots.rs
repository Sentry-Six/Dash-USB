//! Inspect and release XFS reflink snapshots through the runtime's shared cleanup.

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
            // A missing directory represents zero snapshots.
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
        // file_type() reads the dirent and does NOT follow symlinks, so a
        // planted `snap-NNNNNN` link cannot be listed as a snapshot.
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }

        // snap.bin mtime is stable; directory mtime changes with TOC/mount entries.
        let created_unix = std::fs::symlink_metadata(path.join("snap.bin"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // st_blocks is only an upper bound for shared reflink extents.
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

    // Exclusive snapshot footprint is partition usage minus non-snapshot usage;
    // summing reflink file blocks would count shared extents repeatedly.
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

/// Release a validated `snap-*` id through the shared runtime cleanup.
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

    // Prefer shared unmount/symlink cleanup; plain removal is an install fallback.
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
