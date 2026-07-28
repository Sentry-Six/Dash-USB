pub mod auth;
pub mod router;
pub mod status;
pub mod system;
pub mod files;
pub mod terminal;
pub mod notifications;
pub mod notification_center;
pub mod setup;
pub mod archive_mount_lock;
pub mod archive_state;
pub mod backup;
pub mod update;
pub mod support;
pub mod healthcheck;
pub mod clips;
pub mod preferences;
pub mod profile;
pub mod memory;
pub mod logs;
pub mod devices;
pub mod snapshots;
pub mod storage_repair;

pub use auth::{AuthState, init_auth};
pub use router::build_router;

use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;

pub fn json_response<T: Serialize>(status: StatusCode, data: T) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::to_value(data).unwrap_or_default()))
}

pub fn json_error(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({"error": msg})))
}

pub fn json_ok() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(serde_json::json!({"success": true})))
}

/// Process-wide `reqwest` client for the outbound community and notification
/// proxies, so the TLS stack and connection pool are reused across requests.
///
/// Each call site sets its own per-endpoint `.timeout(..)` on the request
/// builder, which overrides the client default. The 120s builder timeout is
/// only a backstop so a site that forgets one can't hang a connection forever.
pub fn http_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}
