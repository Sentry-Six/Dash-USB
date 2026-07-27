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

/// Canonical longitude in [-180, 180). Leaflet lets clients pan into repeated
/// world copies and submit values like -221.4 for 138.6°E, so wrap on write AND
/// on read: stored legacy values must not rehydrate out of range in the UI.
/// Haversine tolerates ±360, so this is storage/display hygiene, not geofence
/// correctness. Must stay in step with the web's `normalizeLon`.
pub fn normalize_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
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

#[cfg(test)]
mod tests {
    use super::normalize_lon;

    #[test]
    fn normalize_lon_wraps_world_copies() {
        // The issue-#159 case: Japan clicked on the previous world copy.
        assert!((normalize_lon(-221.5) - 138.5).abs() < 1e-9);
        assert!((normalize_lon(538.5) - 178.5).abs() < 1e-9);
        assert!((normalize_lon(-540.0) - (-180.0)).abs() < 1e-9);
        assert!((normalize_lon(540.0) - (-180.0)).abs() < 1e-9);
    }

    #[test]
    fn normalize_lon_half_open_range() {
        // Convention: [-180, 180), so exact +180 canonicalizes to -180.
        assert!((normalize_lon(180.0) - (-180.0)).abs() < 1e-9);
        assert!((normalize_lon(-180.0) - (-180.0)).abs() < 1e-9);
    }

    #[test]
    fn normalize_lon_in_range_unchanged() {
        for v in [-179.999, -98.35, 0.0, 138.6, 179.999] {
            assert!((normalize_lon(v) - v).abs() < 1e-9);
        }
    }
}
