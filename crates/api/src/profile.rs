//! Web-facing camera, playback, filename, and timing fields from the active profile.

use axum::http::StatusCode;
use axum::Json;

use sentryusb_vehicle_profile::Profile;

/// Every selectable vehicle with the drive facts the setup wizard needs.
fn profiles_json(active_id: &str) -> serde_json::Value {
    let profiles: Vec<serde_json::Value> = Profile::all_embedded()
        .iter()
        .map(|p| {
            // What the vehicle saves, continuous recordings first; the
            // wizard lets the owner choose which of these to archive.
            let continuous = serde_json::json!({
                "category": sentryusb_vehicle_profile::CONTINUOUS_CATEGORY,
                "label": "Recordings",
            });
            let events = p.events.iter().map(|e| {
                serde_json::json!({ "category": e.category, "label": e.label })
            });
            let categories: Vec<serde_json::Value> = std::iter::once(continuous).chain(events).collect();
            serde_json::json!({
                "categories": categories,
                "id": p.profile.id,
                "display_name": p.profile.display_name,
                "brand": p.profile.brand,
                "experimental": p.profile.experimental,
                "default_size": p.virtual_drive.default_size,
                "min_size": p.virtual_drive.min_size,
                "rolling_window_minutes": p.recording.rolling_window_minutes,
                "approx_gb_per_hour": p.approx_gb_per_hour(),
            })
        })
        .collect();
    serde_json::json!({ "active": active_id, "profiles": profiles })
}

pub async fn list_profiles() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(profiles_json(&Profile::active().profile.id)))
}

pub async fn get_profile() -> (StatusCode, Json<serde_json::Value>) {
    let p = Profile::active();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": p.profile.id,
            "display_name": p.profile.display_name,
            "brand": p.profile.brand,
            "cameras": p.cameras,
            "grid": p.viewer.grid,
            "filename_regex": p.recording.filename_regex,
            "segment_seconds": p.recording.segment_seconds,
            "rolling_window_minutes": p.recording.rolling_window_minutes,
            "events": p.events.iter().map(|e| serde_json::json!({
                "category": e.category,
                "label": e.label,
                "segment_seconds": e.segment_seconds,
            })).collect::<Vec<_>>(),
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_vehicle_with_its_drive_requirements() {
        let value = profiles_json("gm_surroundvision");
        assert_eq!(value["active"], "gm_surroundvision");
        let profiles = value["profiles"].as_array().unwrap();
        let by_id = |id: &str| profiles.iter().find(|p| p["id"] == id).unwrap();

        let gm = by_id("gm_surroundvision");
        assert_eq!(gm["min_size"], "64G");
        assert_eq!(gm["experimental"], false);
        assert_eq!(gm["rolling_window_minutes"], 120);
        // Four always-present cameras at ~111 MB per five minutes.
        assert!((gm["approx_gb_per_hour"].as_f64().unwrap() - 5.3).abs() < 0.1);

        let rivian = by_id("rivian_roadcam");
        assert_eq!(rivian["min_size"], "128G");
        assert_eq!(rivian["experimental"], true);
        assert_eq!(rivian["rolling_window_minutes"], 0);
        assert!(rivian["approx_gb_per_hour"].is_null());

        // Categories the owner can choose to archive, continuous first.
        let labels = |p: &serde_json::Value| -> Vec<String> {
            p["categories"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| format!("{}={}", c["category"].as_str().unwrap(), c["label"].as_str().unwrap()))
                .collect()
        };
        assert_eq!(labels(gm), ["Continuous=Recordings"]);
        assert_eq!(
            labels(rivian),
            [
                "Continuous=Recordings",
                "Incidents=Incidents",
                "Saved=Saved Clips",
                "GearGuard=Gear Guard",
                "Launch=Launch Cam",
                "Exports=Exports",
            ]
        );
    }
}
