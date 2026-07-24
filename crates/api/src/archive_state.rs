//! GET /api/archive/status — live archive progress for the dashboard.
//!
//! archiveloop's progress monitor persists `/tmp/archive_status.json`
//! (`{"phase":"archiving","current":N,"total":M}`) every ~5 s while a
//! batch runs and clears it when the batch ends. Absent, stale (>120 s
//! — a crashed loop), or unparseable ⇒ `{"phase":"idle"}`.

use axum::http::StatusCode;
use axum::Json;

const STATUS_FILE: &str = "/tmp/archive_status.json";

pub async fn get_archive_status() -> (StatusCode, Json<serde_json::Value>) {
    let status = read_archive_status()
        .unwrap_or_else(|| serde_json::json!({ "phase": "idle" }));
    (StatusCode::OK, Json(status))
}

/// Read and parse the status file, returning None if absent, stale, or
/// invalid. Removes the file when its mtime is older than 120 s so a
/// crashed archiveloop can't leave the UI showing "archiving" forever.
fn read_archive_status() -> Option<serde_json::Value> {
    let meta = std::fs::metadata(STATUS_FILE).ok()?;
    if let Ok(modified) = meta.modified() {
        if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
            if age > std::time::Duration::from_secs(120) {
                let _ = std::fs::remove_file(STATUS_FILE);
                return None;
            }
        }
    }
    let data = std::fs::read_to_string(STATUS_FILE).ok()?;
    serde_json::from_str(&data).ok()
}
