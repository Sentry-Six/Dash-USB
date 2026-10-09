//! Durable key-value preferences. Read-modify-write operations hold
//! `PREFS_LOCK`; saves use atomic replacement.

use std::sync::Mutex;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::router::AppState;

/// `/mutable` on the Pi; `DASHUSB_MUTABLE_DIR` overrides it for off-Pi runs.
pub(crate) fn prefs_file() -> String {
    format!("{}/.dashusb_preferences.json", sentryusb_config::mutable_dir())
}
/// Legacy path, read-only fallback so upgrades don't lose existing prefs.
fn legacy_prefs_file() -> String {
    format!("{}/dashusb-prefs.json", sentryusb_config::mutable_dir())
}

/// Serializes the read-modify-write in `set_preference` so interleaved PUTs
/// can't lose updates.
static PREFS_LOCK: Mutex<()> = Mutex::new(());

type Preferences = serde_json::Map<String, serde_json::Value>;

fn read_preferences(path: &std::path::Path, legacy: &std::path::Path) -> std::io::Result<Preferences> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match std::fs::read(legacy) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Preferences::new()),
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&data).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn write_preferences(path: &std::path::Path, prefs: &Preferences) -> std::io::Result<()> {
    use std::io::Write;
    let data = serde_json::to_vec_pretty(prefs)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_nanos();
    let temporary = path.with_extension(format!("{}.{nonce}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let result = (|| {
        file.write_all(&data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().unwrap_or(std::path::Path::new(".")))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(temporary); }
    result
}

/// Serialize read/modify/write and fail without replacing unreadable preferences.
pub(crate) fn edit_prefs(edit: impl FnOnce(&mut Preferences)) -> std::io::Result<()> {
    let _guard = PREFS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = prefs_file();
    let mut prefs = read_preferences(std::path::Path::new(&path), std::path::Path::new(&legacy_prefs_file()))?;
    edit(&mut prefs);
    write_preferences(std::path::Path::new(&path), &prefs)
}

pub(crate) fn read_prefs() -> std::io::Result<Preferences> {
    read_preferences(std::path::Path::new(&prefs_file()), std::path::Path::new(&legacy_prefs_file()))
}

pub(crate) fn load_prefs() -> serde_json::Map<String, serde_json::Value> {
    read_prefs().unwrap_or_default()
}

pub(crate) fn save_prefs(prefs: &serde_json::Map<String, serde_json::Value>) {
    if let Err(error) = write_preferences(std::path::Path::new(&prefs_file()), prefs) {
        tracing::warn!("[preferences] failed to save: {}", error);
    }
}

#[derive(Deserialize)]
pub struct PrefQuery {
    key: Option<String>,
}

pub async fn get_preference(
    State(_s): State<AppState>,
    Query(params): Query<PrefQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let prefs = match read_prefs() {
        Ok(prefs) => prefs,
        Err(_) => return crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Preferences could not be read."),
    };
    if let Some(key) = &params.key {
        let val = prefs.get(key).cloned().unwrap_or(serde_json::Value::Null);
        (StatusCode::OK, Json(serde_json::json!({"key": key, "value": val})))
    } else {
        (StatusCode::OK, Json(serde_json::Value::Object(prefs)))
    }
}

pub async fn set_preference(
    State(_s): State<AppState>,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    #[derive(Deserialize)]
    struct SetReq {
        key: String,
        value: serde_json::Value,
    }

    let req: SetReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return crate::json_error(StatusCode::BAD_REQUEST, "invalid request body"),
    };

    match tokio::task::spawn_blocking(move || edit_prefs(|prefs| { prefs.insert(req.key, req.value); })).await {
        Ok(Ok(())) => crate::json_ok(),
        _ => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Preference could not be saved."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_preferences_do_not_fall_back_to_old_values() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current.json");
        let legacy = dir.path().join("legacy.json");
        std::fs::write(&legacy, r#"{"old":true}"#).unwrap();
        assert_eq!(read_preferences(&current, &legacy).unwrap()["old"], true);
        std::fs::write(&current, "{broken").unwrap();
        assert!(read_preferences(&current, &legacy).is_err());
        assert_eq!(std::fs::read_to_string(current).unwrap(), "{broken");
    }

    #[test]
    fn preference_write_is_atomic_and_cleans_up_a_failed_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Preferences::from_iter([("unrelated".into(), serde_json::json!("keep"))]);
        write_preferences(&path, &prefs).unwrap();
        assert_eq!(read_preferences(&path, &path).unwrap(), prefs);
        let blocked = dir.path().join("directory.json");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), "unchanged").unwrap();
        assert!(write_preferences(&blocked, &prefs).is_err());
        assert_eq!(std::fs::read_to_string(blocked.join("keep")).unwrap(), "unchanged");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
