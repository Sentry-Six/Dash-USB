//! Clip listing.

use std::path::Path;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::router::AppState;

const RECORDINGS_DIR: &str = "/mutable/Recordings";

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
}

/// Dated category folders newest-first, following snapshot symlinks.
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
/// category. Each clip group is a dated subfolder of `.mp4` files.
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

        entries.push(ClipEntry {
            date: dir_name.clone(),
            path: format!("/Recordings/{}/{}", category, dir_name),
            files,
        });
    }

    serde_json::json!([{
        "name": category,
        "clips": entries,
        "hasMore": has_more,
    }])
}

/// Query params: `category` (only `Continuous` is accepted), `limit` (default
/// 20, capped at 200), and a `before` date cursor.
pub async fn get_clips(
    State(_s): State<AppState>,
    Query(params): Query<ClipParams>,
) -> (StatusCode, Json<serde_json::Value>) {
    let category = params.category.as_deref().unwrap_or("Continuous");
    if !matches!(category, "Continuous") {
        return crate::json_error(StatusCode::BAD_REQUEST, "invalid category");
    }
    let limit = params.limit.unwrap_or(20).min(200);

    // Autofs-backed clip reads may block; keep them off async workers.
    let category = category.to_string();
    let before = params.before;
    let response = tokio::task::spawn_blocking(move || {
        list_clips_in(Path::new(RECORDINGS_DIR), &category, limit, before.as_deref())
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

    /// Continuous recordings are date-bucketed under `/mutable/Recordings`.
    #[test]
    fn lists_continuous_clips_from_dated_subdirs() {
        let root = TempDir::new().unwrap();
        let day = root.path().join("Continuous").join("2026-07-17");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("FRONT_2026_07_17_T_19_34_53.mp4"), b"").unwrap();
        fs::write(day.join("REAR_2026_07_17_T_19_34_53.mp4"), b"").unwrap();

        let value = list_clips_in(root.path(), "Continuous", 20, None);
        assert_eq!(value[0]["name"].as_str().unwrap(), "Continuous");
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);

        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2026-07-17");
        assert_eq!(
            clips[0]["path"].as_str().unwrap(),
            "/Recordings/Continuous/2026-07-17",
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
                "FRONT_2026_07_17_T_19_34_53.mp4",
                "REAR_2026_07_17_T_19_34_53.mp4",
            ],
        );
        assert!(clips[0].get("event").is_none());
    }

    #[test]
    fn list_clips_respects_limit_and_before() {
        let root = TempDir::new().unwrap();
        let saved = root.path().join("Continuous");
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
        let value = list_clips_in(root.path(), "Continuous", 2, None);
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), true);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-22_10-00-00");
        assert_eq!(clips[1]["date"].as_str().unwrap(), "2025-02-21_10-00-00");

        // `before` cursor drops entries at or after the cursor.
        let value = list_clips_in(root.path(), "Continuous", 20, Some("2025-02-22_10-00-00"));
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2025-02-21_10-00-00");
        assert_eq!(clips[1]["date"].as_str().unwrap(), "2025-02-20_10-00-00");
    }

    #[test]
    fn list_clips_empty_for_missing_category_dir() {
        let root = TempDir::new().unwrap();
        let value = list_clips_in(root.path(), "Continuous", 20, None);
        assert_eq!(value[0]["name"].as_str().unwrap(), "Continuous");
        assert_eq!(value[0]["clips"].as_array().unwrap().len(), 0);
        assert_eq!(value[0]["hasMore"].as_bool().unwrap(), false);
    }

    /// Clip directories may be symlinks into reflink snapshots.
    #[cfg(unix)]
    #[test]
    fn list_clips_follows_symlinked_dirs() {
        let root = TempDir::new().unwrap();
        let cont = root.path().join("Continuous");
        fs::create_dir_all(&cont).unwrap();

        // A real clip dir living outside the category folder...
        let real = root.path().join("snapshot").join("2026-07-17");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("FRONT_2026_07_17_T_19_34_53.mp4"), b"").unwrap();

        // ...reachable only through a symlink inside Continuous/.
        std::os::unix::fs::symlink(&real, cont.join("2026-07-17")).unwrap();

        let value = list_clips_in(root.path(), "Continuous", 20, None);
        let clips = value[0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0]["date"].as_str().unwrap(), "2026-07-17");
        let files: Vec<&str> = clips[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(files, vec!["FRONT_2026_07_17_T_19_34_53.mp4"]);
    }
}
