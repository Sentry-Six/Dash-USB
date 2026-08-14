//! Dashboard archive progress from archiveloop's temporary status file.

use axum::http::StatusCode;
use axum::Json;

const STATUS_FILE: &str = "/tmp/archive_status.json";

pub async fn get_archive_status() -> (StatusCode, Json<serde_json::Value>) {
    let status = read_archive_status()
        .unwrap_or_else(|| serde_json::json!({ "phase": "idle" }));
    (StatusCode::OK, Json(status))
}

/// Reject and remove status older than 120 seconds.
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
