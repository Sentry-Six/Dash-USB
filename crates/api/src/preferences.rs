//! User preferences (key-value store).
//!
//! Concurrency: [`set_preference`] is a read-modify-write and MUST hold
//! `PREFS_LOCK` for the whole sequence. Without it two concurrent PUTs read the
//! same baseline and the second write silently clobbers the first.
//!
//! Durability: saves go through tmp+rename so a power cut mid-write can't leave
//! the file half-formed, parseable as empty and losing every stored flag.

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

pub(crate) fn load_prefs() -> serde_json::Map<String, serde_json::Value> {
    if let Ok(d) = std::fs::read_to_string(prefs_file()) {
        if let Ok(v) = serde_json::from_str(&d) {
            return v;
        }
    }
    std::fs::read_to_string(legacy_prefs_file())
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default()
}

pub(crate) fn save_prefs(prefs: &serde_json::Map<String, serde_json::Value>) {
    // tmp+rename: a direct `fs::write` leaves a zero-length file if the kernel
    // panics mid-write, which silently resets every toggle (notification
    // settings, update channel, analytics opt-in) to its default on next boot.
    //
    // The first-install wizard saves prefs BEFORE the /mutable partition exists
    // and is mounted, so pre-create the parent or the write fails with ENOENT.
    // That placeholder lands on rootfs; later saves land on the real partition.
    let data = serde_json::to_string_pretty(prefs).unwrap_or_default();
    let prefs_path = prefs_file();
    if let Some(parent) = std::path::Path::new(&prefs_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = format!("{}.tmp", prefs_path);
    if let Err(e) = std::fs::write(&tmp, &data) {
        tracing::warn!("[preferences] failed to write tmp: {}", e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &prefs_path) {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("[preferences] failed to rename into place: {}", e);
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
    let prefs = load_prefs();
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

    {
        // Hold across the whole load, modify, save. Recovering from a poisoned
        // guard is safe: every save rewrites the file from a complete
        // in-memory map.
        let _guard = PREFS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut prefs = load_prefs();
        prefs.insert(req.key, req.value);
        save_prefs(&prefs);
    }

    crate::json_ok()
}

