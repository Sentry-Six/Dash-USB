//! Dashboard archive progress from archiveloop's temporary status file.

use axum::http::StatusCode;
use axum::Json;

const STATUS_FILE: &str = "/tmp/archive_status.json";

pub async fn get_archive_status() -> (StatusCode, Json<serde_json::Value>) {
    let mut status = read_archive_status()
        .unwrap_or_else(|| serde_json::json!({ "phase": "idle" }));
    let control = crate::archive_control::ArchiveControl::default();
    if let Some(id) = control.active_cycle() {
        status["cycle"] = serde_json::json!({"cancelling": control.cycle_cancelled(&id), "id": id});
    }
    (StatusCode::OK, Json(status))
}

#[derive(serde::Deserialize)]
pub struct CancelArchiveRequest { cycle_id: String }

/// Acknowledge only after protecting unarchived footage against cleanup.
pub async fn cancel_archive(Json(request): Json<CancelArchiveRequest>) -> (StatusCode, Json<serde_json::Value>) {
    let result = tokio::task::spawn_blocking(move ||
        crate::archive_control::ArchiveControl::default().request_cancel(&request.cycle_id)
    ).await.unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    match result {
        Ok(_guard) => (StatusCode::ACCEPTED, Json(serde_json::json!({"success": true}))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            crate::json_error(StatusCode::CONFLICT, "Archive cycle has already ended; refresh status"),
        Err(error) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
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
    parse_status(&data)
}

fn parse_status(data: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    value.is_object().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::parse_status;

    #[test]
    fn corrupted_status_cannot_panic_when_cycle_metadata_is_added() {
        for input in ["null", "[]", "42", "\"archiving\"", "{broken"] {
            assert!(parse_status(input).is_none());
        }
        let mut status = parse_status(r#"{"phase":"archiving","current":2,"total":10}"#).unwrap();
        status["cycle"] = serde_json::json!({"id":"1:abc","cancelling":false});
        assert_eq!(status["current"], 2);
    }
}
