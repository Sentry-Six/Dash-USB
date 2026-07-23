//! Clip listing and telemetry.

use std::path::Path;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::router::AppState;

const TESLACAM_DIR: &str = "/mutable/TeslaCam";

#[derive(Deserialize)]
pub struct ClipParams {
    category: Option<String>,
    limit: Option<usize>,
    before: Option<String>,
}

#[derive(Serialize)]
struct ClipEntry {
    date: String,
    path: String,
    files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<EventMeta>,
}

#[derive(Serialize, Deserialize)]
struct EventMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    camera: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latitude: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    longitude: Option<String>,
}

/// Read a `RecentClips/`, `SavedClips/`, or `SentryClips/` directory and return
/// its dated subfolders, newest first.
///
/// All three categories share this layout under `/mutable/TeslaCam`: the
/// snapshot symlink builder (`sentryusb_gadget::snapshot`) date-buckets the
/// car's flat RecentClips files into `YYYY-MM-DD/` folders, matching the
/// `YYYY-MM-DD_HH-MM-SS/` event folders SavedClips/SentryClips already use.
/// `path().is_dir()` follows symlinks — required, since each entry is a
/// symlink into a reflink snapshot.
fn enumerate_event_dirs(base: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = match std::fs::read_dir(base) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => return Vec::new(),
    };
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

/// Build the `[{ name, clips, hasMore }]` JSON the Viewer expects for one
/// category:
/// one code path for all three categories — each clip is a dated subfolder of
/// `.mp4` files plus an optional `event.json`.
fn list_clips_in(
    teslacam_dir: &Path,
    category: &str,
    limit: usize,
    before: Option<&str>,
) -> serde_json::Value {
    let base = teslacam_dir.join(category);
    if !base.exists() {
        return serde_json::json!([{
            "name": category,
            "clips": [],
            "hasMore": false,
        }]);
    }

    let mut event_dirs = enumerate_event_dirs(&base);
    if let Some(before) = before {
        event_dirs.retain(|d| d.as_str() < before);
    }
    let has_more = event_dirs.len() > limit;
    event_dirs.truncate(limit);

    let mut entries = Vec::with_capacity(event_dirs.len());
    for dir_name in event_dirs {
        let dir_path = base.join(&dir_name);
        let mut files = Vec::new();
        if let Ok(items) = std::fs::read_dir(&dir_path) {
            for item in items.flatten() {
                let name = item.file_name().to_string_lossy().to_string();
                if name.ends_with(".mp4") {
                    files.push(name);
                }
            }
        }
        files.sort();

        let event = std::fs::read_to_string(dir_path.join("event.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<EventMeta>(&s).ok());

        entries.push(ClipEntry {
            date: dir_name.clone(),
            path: format!("/TeslaCam/{}/{}", category, dir_name),
            files,
            event,
        });
    }

    serde_json::json!([{
        "name": category,
        "clips": entries,
        "hasMore": has_more,
    }])
}

/// GET /api/clips?category=RecentClips&limit=20[&before=<date>]
pub async fn get_clips(
    State(_s): State<AppState>,
    Query(params): Query<ClipParams>,
) -> (StatusCode, Json<serde_json::Value>) {
    let category = params.category.as_deref().unwrap_or("SavedClips");
    if !matches!(category, "SavedClips" | "SentryClips" | "RecentClips") {
        return crate::json_error(StatusCode::BAD_REQUEST, "invalid category");
    }
    let limit = params.limit.unwrap_or(20).min(200);

    // Each dated clip folder is read off disk, and the folders are
    // symlinks into on-demand (autofs) snapshot mounts — the first read
    // can block for seconds while the kernel mounts the image. Run it on
    // the blocking pool so it can't stall the async reactor and drop the
    // WebSocket heartbeat ("Reconnecting to SentryUSB…").
    let category = category.to_string();
    let before = params.before;
    let response = tokio::task::spawn_blocking(move || {
        list_clips_in(Path::new(TESLACAM_DIR), &category, limit, before.as_deref())
    })
    .await
    .unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn enumerate_event_dirs_returns_subfolders_newest_first() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("2025-02-22_17-58-00")).unwrap();
        fs::create_dir(dir.path().join("2025-02-23_09-12-00")).unwrap();
        // Stray file should be ignored
        fs::write(dir.path().join("README.txt"), b"").unwrap();

        let dirs = enumerate_event_dirs(dir.path());
        assert_eq!(dirs, vec!["2025-02-23_09-12-00", "2025-02-22_17-58-00"]);
    }

    /// RecentClips under `/mutable/TeslaCam` are dated `YYYY-MM-DD/` subfolders
    /// (the snapshot symlink builder date-buckets them), not flat files — so
    /// they list through the same code path as SavedClips/SentryClips.
    #[test]
    fn lists_recent_clips_from_dated_subdirs() {
        let root = TempDir::new().unwrap();
        let day = root.path().join("RecentClips").join("2025-02-22");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("2025-02-22_17-58-00-front.mp4"), b"").unwrap();
        fs::write(day.join("2025-02-22_17-58-00-back.mp4"), b"").unwrap();

        let value = list_clips_in(root.path(), "RecentClips", 20, None);
        assert_eq!(value[0]["name"].as_str().unwrap(), "RecentClips");
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);

        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-22");
        assert_eq!(
            clips[0]["path"].as_str().unwrap(),
            "/TeslaCam/RecentClips/2025-02-22",
        );
        let files: Vec<&str> = clips[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            files,
            vec![
                "2025-02-22_17-58-00-back.mp4",
                "2025-02-22_17-58-00-front.mp4",
            ],
        );
        // RecentClips carry no event.json, so `event` is skipped entirely.
        assert!(clips[0].get("event").is_none());
    }

    #[test]
    fn lists_event_clips_with_event_json() {
        let root = TempDir::new().unwrap();
        let event = root.path().join("SentryClips").join("2025-02-22_17-58-00");
        fs::create_dir_all(&event).unwrap();
        fs::write(event.join("2025-02-22_17-58-00-front.mp4"), b"").unwrap();
        fs::write(
            event.join("event.json"),
            r#"{"city":"San Francisco, CA","reason":"sentry_aware_object_detection"}"#,
        )
        .unwrap();

        let value = list_clips_in(root.path(), "SentryClips", 20, None);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-22_17-58-00");
        assert_eq!(
            clips[0]["path"].as_str().unwrap(),
            "/TeslaCam/SentryClips/2025-02-22_17-58-00",
        );
        assert_eq!(clips[0]["event"]["city"].as_str().unwrap(), "San Francisco, CA");
        assert_eq!(
            clips[0]["event"]["reason"].as_str().unwrap(),
            "sentry_aware_object_detection",
        );
    }

    #[test]
    fn list_clips_respects_limit_and_before() {
        let root = TempDir::new().unwrap();
        let saved = root.path().join("SavedClips");
        for name in &[
            "2025-02-20_10-00-00",
            "2025-02-21_10-00-00",
            "2025-02-22_10-00-00",
        ] {
            let d = saved.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(format!("{}-front.mp4", name)), b"").unwrap();
        }

        // `limit` truncates and reports hasMore, newest first.
        let value = list_clips_in(root.path(), "SavedClips", 2, None);
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), true);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-22_10-00-00");
        assert_eq!(clips[1]["date"].as_str().unwrap(), "2025-02-21_10-00-00");

        // `before` cursor drops entries at or after the cursor.
        let value = list_clips_in(root.path(), "SavedClips", 20, Some("2025-02-22_10-00-00"));
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-21_10-00-00");
        assert_eq!(clips[1]["date"].as_str().unwrap(), "2025-02-20_10-00-00");
    }

    #[test]
    fn list_clips_empty_for_missing_category_dir() {
        let root = TempDir::new().unwrap();
        let value = list_clips_in(root.path(), "SavedClips", 20, None);
        assert_eq!(value[0]["name"].as_str().unwrap(), "SavedClips");
        assert_eq!(value[0]["clips"].as_array().unwrap().len(), 0);
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);
    }

    /// The real `/mutable/TeslaCam` clip entries are symlinks into reflink
    /// snapshots, so `enumerate_event_dirs` must follow symlinked directories.
    #[cfg(unix)]
    #[test]
    fn list_clips_follows_symlinked_dirs() {
        let root = TempDir::new().unwrap();
        let saved = root.path().join("SavedClips");
        fs::create_dir_all(&saved).unwrap();

        // A real clip dir living outside the category folder...
        let real = root.path().join("snapshot").join("2025-02-22_17-58-00");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("2025-02-22_17-58-00-front.mp4"), b"").unwrap();

        // ...reachable only through a symlink inside SavedClips/.
        std::os::unix::fs::symlink(&real, saved.join("2025-02-22_17-58-00")).unwrap();

        let value = list_clips_in(root.path(), "SavedClips", 20, None);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-22_17-58-00");
        let files: Vec<&str> = clips[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(files, vec!["2025-02-22_17-58-00-front.mp4"]);
    }
}
