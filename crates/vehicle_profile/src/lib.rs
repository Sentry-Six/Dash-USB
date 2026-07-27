//! Vehicle profiles: how a car brand records dashcam footage onto the USB
//! drive (recording path, filename format, camera set, segment length,
//! rolling-delete window, virtual-drive geometry).
//!
//! Profiles are TOML under `profiles/`, embedded into the binary at
//! compile time so an OTA update can never skew the binary against its
//! profile. Add a brand by adding a file and listing it in [`EMBEDDED`].
//!
//! `VEHICLE_PROFILE` in dashusb.conf selects an embedded profile by id;
//! `DASHUSB_PROFILE_PATH` overrides with an on-disk TOML for dev/bench.
//! Anything invalid falls back to the default with a logged warning: the
//! recorder must never fail to boot over a bad profile reference.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PROFILE_ID: &str = "gm_surroundvision";

/// Compiled-in profiles, keyed by `profile.id`.
const EMBEDDED: &[(&str, &str)] = &[(
    "gm_surroundvision",
    include_str!("../../../profiles/gm_surroundvision.toml"),
)];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub profile: Meta,
    pub recording: Recording,
    pub cameras: Vec<Camera>,
    pub viewer: Viewer,
    pub virtual_drive: VirtualDrive,
    pub snapshots: Snapshots,
    pub features: Features,
    #[serde(skip)]
    compiled_regex: OnceLock<regex::Regex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub display_name: String,
    pub brand: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    /// Recording root inside the virtual drive, relative, no leading slash.
    pub root: String,
    /// Must expose named captures `camera`, `y`, `mo`, `d`, `h`, `mi`, `s`.
    pub filename_regex: String,
    pub segment_seconds: u32,
    pub rolling_window_minutes: u32,
    pub approx_bytes_per_camera_segment: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Camera {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewer {
    /// Row-major camera grid; empty string = empty cell.
    pub grid: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualDrive {
    pub default_size: String,
    pub min_size: String,
    /// Free-space floor (bytes) the post-archive cleaner leaves so the
    /// car's available-space check keeps passing.
    pub min_free_bytes: u64,
    /// Only "fat32" is supported today; the field exists so a brand with
    /// a different on-drive format stays data-only.
    pub filesystem: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshots {
    pub default_interval_secs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    pub event_folders: bool,
    pub archive_everything_default: bool,
    pub nofua: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipInfo {
    /// Camera id exactly as captured (e.g. "FRONT").
    pub camera: String,
    pub timestamp: chrono::NaiveDateTime,
}

impl ClipInfo {
    /// Date bucket for the recordings tree ("2026-07-17").
    pub fn date_str(&self) -> String {
        self.timestamp.format("%Y-%m-%d").to_string()
    }

    /// Cross-camera grouping key: all cameras of one segment share it.
    pub fn group_key(&self) -> String {
        self.timestamp.format("%Y-%m-%d_%H-%M-%S").to_string()
    }
}

impl Profile {
    pub fn from_toml(s: &str) -> Result<Self> {
        let p: Profile = toml::from_str(s).context("parsing vehicle profile TOML")?;
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<()> {
        let re = regex::Regex::new(&self.recording.filename_regex)
            .context("filename_regex does not compile")?;
        for cap in ["camera", "y", "mo", "d", "h", "mi", "s"] {
            anyhow::ensure!(
                re.capture_names().flatten().any(|n| n == cap),
                "filename_regex is missing the named capture `{cap}`"
            );
        }
        for row in &self.viewer.grid {
            for cell in row.iter().filter(|c| !c.is_empty()) {
                anyhow::ensure!(
                    self.cameras.iter().any(|c| &c.id == cell),
                    "viewer.grid references unknown camera `{cell}`"
                );
            }
        }
        anyhow::ensure!(
            self.recording.root.starts_with(|c: char| c.is_ascii_alphanumeric()),
            "recording.root must be a relative path"
        );
        Ok(())
    }

    pub fn embedded(id: &str) -> Option<Result<Self>> {
        EMBEDDED
            .iter()
            .find(|(pid, _)| *pid == id)
            .map(|(_, s)| Self::from_toml(s))
    }

    /// The process-wide active profile.
    ///
    /// Resolution: `DASHUSB_PROFILE_PATH` env (dev/bench override), then
    /// the `VEHICLE_PROFILE` conf key, then the default. Every failure
    /// path logs and falls back to the embedded default, which unit tests
    /// guarantee parses.
    pub fn active() -> &'static Profile {
        static ACTIVE: OnceLock<Profile> = OnceLock::new();
        ACTIVE.get_or_init(|| {
            if let Ok(path) = std::env::var("DASHUSB_PROFILE_PATH") {
                match std::fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|s| Self::from_toml(&s))
                {
                    Ok(p) => {
                        tracing::info!("vehicle profile: {} (from {})", p.profile.id, path);
                        return p;
                    }
                    Err(e) => {
                        tracing::warn!("DASHUSB_PROFILE_PATH={path} unusable ({e:#}); falling back")
                    }
                }
            }
            let (active, _) = sentryusb_config::parse_file(sentryusb_config::find_config_path())
                .unwrap_or_default();
            let id = active
                .get("VEHICLE_PROFILE")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_PROFILE_ID)
                .to_string();
            match Self::embedded(&id) {
                Some(Ok(p)) => {
                    tracing::info!("vehicle profile: {}", p.profile.id);
                    return p;
                }
                Some(Err(e)) => tracing::warn!("embedded profile `{id}` invalid ({e:#})"),
                None => tracing::warn!("VEHICLE_PROFILE=`{id}` unknown; using default"),
            }
            Self::embedded(DEFAULT_PROFILE_ID)
                .expect("default profile is embedded")
                .expect("default profile parses (covered by unit test)")
        })
    }

    pub fn clip_regex(&self) -> &regex::Regex {
        self.compiled_regex.get_or_init(|| {
            // validate() already proved this compiles.
            regex::Regex::new(&self.recording.filename_regex).expect("validated regex")
        })
    }

    /// Parse a bare clip filename (no path components) into camera + timestamp.
    pub fn parse_clip_filename(&self, name: &str) -> Option<ClipInfo> {
        let caps = self.clip_regex().captures(name)?;
        let num = |k: &str| caps.name(k).and_then(|m| m.as_str().parse::<u32>().ok());
        let date = NaiveDate::from_ymd_opt(num("y")? as i32, num("mo")?, num("d")?)?;
        let ts = date.and_hms_opt(num("h")?, num("mi")?, num("s")?)?;
        Some(ClipInfo {
            camera: caps.name("camera")?.as_str().to_string(),
            timestamp: ts,
        })
    }

    /// Render `/root/bin/profile_env.sh`, the bridge handing archiveloop
    /// and the per-method archive scripts the profile values they need.
    /// Values a dashusb.conf key may override carry a `*_DEFAULT` suffix
    /// and are read as `${SNAPSHOT_INTERVAL:-$SNAPSHOT_INTERVAL_DEFAULT}`,
    /// so user config always wins.
    pub fn render_profile_env(&self) -> String {
        format!(
            "#!/bin/bash\n\
             # Generated by the dashusb daemon from the active vehicle profile\n\
             # ({id}) at every startup — do not edit; changes are overwritten.\n\
             export VEHICLE_PROFILE_ID={id}\n\
             export RECORDINGS_TREE=/mutable/Recordings\n\
             export RECORDING_ROOT={root}\n\
             export CAM_MIN_FREE_BYTES={min_free}\n\
             export RECORDINGS_ARCHIVE_DEFAULT={archive}\n\
             export SNAPSHOT_INTERVAL_DEFAULT={interval}\n\
             export CLIP_MIN_BYTES=100000\n",
            id = self.profile.id,
            root = self.recording.root,
            min_free = self.virtual_drive.min_free_bytes,
            archive = self.features.archive_everything_default,
            interval = self.snapshots.default_interval_secs,
        )
    }
}

/// Write `/root/bin/profile_env.sh` when its content differs. Runs on
/// every daemon start so OTA updates propagate profile changes without a
/// setup re-run. Skips silently when /root/bin is absent (dev machines).
pub fn write_profile_env() {
    let dir = std::path::Path::new("/root/bin");
    if !dir.is_dir() {
        return;
    }
    let path = dir.join("profile_env.sh");
    let want = Profile::active().render_profile_env();
    if std::fs::read_to_string(&path).map(|cur| cur == want).unwrap_or(false) {
        return;
    }
    if let Err(e) = std::fs::write(&path, &want) {
        tracing::warn!("could not write {}: {e}", path.display());
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    tracing::info!("wrote {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gm() -> Profile {
        Profile::embedded(DEFAULT_PROFILE_ID).unwrap().unwrap()
    }

    #[test]
    fn default_profile_parses_and_validates() {
        let p = gm();
        assert_eq!(p.profile.id, "gm_surroundvision");
        assert_eq!(p.recording.segment_seconds, 300);
        assert_eq!(p.cameras.len(), 5);
        assert!(p.cameras.iter().find(|c| c.id == "INTERIOR").unwrap().optional);
    }

    #[test]
    fn parses_real_gm_filenames() {
        let p = gm();
        let info = p.parse_clip_filename("FRONT_2026_07_17_T_19_34_53.mp4").unwrap();
        assert_eq!(info.camera, "FRONT");
        assert_eq!(info.date_str(), "2026-07-17");
        assert_eq!(info.group_key(), "2026-07-17_19-34-53");

        for name in [
            "LEFT_2026_07_17_T_19_04_53.mp4",
            "RIGHT_2026_07_17_T_19_39_53.mp4",
            "REAR_2026_07_17_T_19_04_19.mp4",
            "INTERIOR_2027_01_02_T_03_04_05.mp4",
        ] {
            assert!(p.parse_clip_filename(name).is_some(), "{name} must parse");
        }
    }

    #[test]
    fn rejects_foreign_and_malformed_names() {
        let p = gm();
        for name in [
            "2024-01-01_12-00-00-front.mp4",     // Tesla format
            "front_2026_07_17_T_19_34_53.mp4",   // lowercase camera
            "FRONT_2026_07_17_19_34_53.mp4",     // missing T separator
            "FRONT_2026_13_40_T_25_61_61.mp4",   // impossible date/time
            "FRONT_2026_07_17_T_19_34_53.mp4.tmp",
            "thumbnail.jpg",
        ] {
            assert!(p.parse_clip_filename(name).is_none(), "{name} must be rejected");
        }
    }

    #[test]
    fn profile_env_render_is_stable() {
        let env = gm().render_profile_env();
        assert!(env.contains("export RECORDINGS_TREE=/mutable/Recordings"));
        assert!(env.contains(
            "export RECORDING_ROOT=Android/media/com.gm.ultifi.gmconnectedcameraservice/Recordings/SurroundVisionRecorder"
        ));
        assert!(env.contains("export CAM_MIN_FREE_BYTES=34359738368"));
        assert!(env.contains("export SNAPSHOT_INTERVAL_DEFAULT=900"));
        assert!(env.contains("export RECORDINGS_ARCHIVE_DEFAULT=true"));
    }
}
