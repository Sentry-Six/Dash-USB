//! Embedded vehicle-specific recording formats and virtual-drive geometry.
//! Invalid selections fall back to the default profile so recording can boot.

use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PROFILE_ID: &str = "gm_surroundvision";

/// Joins path components into a clip name for profiles that set
/// `recording.match_relative_path`.
pub const PATH_COMPONENT_SEP: &str = "__";

/// The profile in use; see [`Profile::active`] and [`Profile::reload`].
static ACTIVE: RwLock<Option<&'static Profile>> = RwLock::new(None);

/// Compiled-in profiles, keyed by `profile.id`.
const EMBEDDED: &[(&str, &str)] = &[
    (
        "gm_surroundvision",
        include_str!("../../../profiles/gm_surroundvision.toml"),
    ),
    (
        "rivian_roadcam",
        include_str!("../../../profiles/rivian_roadcam.toml"),
    ),
];

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
    /// Not yet confirmed in a car; the setup wizard labels it as such.
    #[serde(default)]
    pub experimental: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    /// Recording root inside the virtual drive, relative, no leading slash.
    pub root: String,
    /// Must expose named captures `camera`, `y`, `mo`, `d`, `h`, `mi`, `s`.
    /// A two-digit `y` is read as 20xx.
    pub filename_regex: String,
    /// Match `filename_regex` against the clip's path below `root`, with its
    /// components joined by [`PATH_COMPONENT_SEP`], instead of the bare
    /// filename. For cars that put the timestamp in a per-session folder name.
    /// Recording links take the same joined name, so they stay unique.
    #[serde(default)]
    pub match_relative_path: bool,
    pub segment_seconds: u32,
    /// Age at which the car deletes footage; 0 = it only deletes when the
    /// drive is full.
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
    /// On-drive filesystem; currently only `fat32` is implemented.
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

    /// Process-wide profile resolved from the development override, config,
    /// then the embedded default.
    pub fn active() -> &'static Profile {
        if let Some(p) = *ACTIVE.read().unwrap_or_else(|e| e.into_inner()) {
            return p;
        }
        let mut slot = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = *slot {
            return p;
        }
        let p: &'static Profile = Box::leak(Box::new(Self::resolve()));
        *slot = Some(p);
        p
    }

    /// Re-resolve the active profile after the configuration changed, so a
    /// vehicle picked in the setup wizard applies to the setup run that
    /// follows without a restart. Returns true when the profile changed.
    pub fn reload() -> bool {
        let fresh = Self::resolve();
        let mut slot = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_some_and(|cur| cur.profile.id == fresh.profile.id) {
            return false;
        }
        // Earlier callers may still hold the previous reference, so it is
        // leaked rather than freed; a profile is small and switches are rare.
        *slot = Some(Box::leak(Box::new(fresh)));
        true
    }

    /// Every compiled-in profile that parses, in listing order.
    pub fn all_embedded() -> Vec<Profile> {
        EMBEDDED
            .iter()
            .filter_map(|(_, s)| Self::from_toml(s).ok())
            .collect()
    }

    fn resolve() -> Profile {
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
    }

    /// Approximate recording rate across the always-present cameras, or
    /// `None` when the profile does not know its segment size.
    pub fn approx_gb_per_hour(&self) -> Option<f64> {
        let cameras = self.cameras.iter().filter(|c| !c.optional).count() as f64;
        let per_hour = self.recording.approx_bytes_per_camera_segment as f64 * cameras * 3600.0
            / self.recording.segment_seconds.max(1) as f64;
        (per_hour > 0.0).then_some(per_hour / 1e9)
    }

    pub fn clip_regex(&self) -> &regex::Regex {
        self.compiled_regex.get_or_init(|| {
            // validate() already compiled the same expression.
            regex::Regex::new(&self.recording.filename_regex).expect("validated regex")
        })
    }

    /// Whether the car deletes footage by age rather than only when full.
    pub fn has_rolling_window(&self) -> bool {
        self.recording.rolling_window_minutes > 0
    }

    /// Name a clip is matched and linked under, given its path components
    /// below `recording.root` (file name last).
    pub fn clip_name(&self, rel_components: &[String]) -> Option<String> {
        if self.recording.match_relative_path {
            (!rel_components.is_empty()).then(|| rel_components.join(PATH_COMPONENT_SEP))
        } else {
            rel_components.last().cloned()
        }
    }

    /// Parse a clip name (see [`Profile::clip_name`]) into camera + timestamp.
    pub fn parse_clip_filename(&self, name: &str) -> Option<ClipInfo> {
        let caps = self.clip_regex().captures(name)?;
        let num = |k: &str| caps.name(k).and_then(|m| m.as_str().parse::<u32>().ok());
        let year = match caps.name("y")?.as_str().len() {
            2 => 2000 + num("y")?,
            _ => num("y")?,
        };
        let date = NaiveDate::from_ymd_opt(year as i32, num("mo")?, num("d")?)?;
        let ts = date.and_hms_opt(num("h")?, num("mi")?, num("s")?)?;
        Some(ClipInfo {
            camera: caps.name("camera")?.as_str().to_string(),
            timestamp: ts,
        })
    }

    /// Render the profile environment consumed by archiveloop. Configurable
    /// values use `*_DEFAULT` names so user configuration takes precedence.
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

/// Pick up a vehicle-profile change in the saved configuration: switch the
/// active profile and regenerate archiveloop's environment.
pub fn apply_config_change() {
    if Profile::reload() {
        write_profile_env();
    }
}

/// Refresh `/root/bin/profile_env.sh` when profile output changes.
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
        assert_eq!(
            info.timestamp.format("%Y-%m-%d_%H-%M-%S").to_string(),
            "2026-07-17_19-34-53"
        );

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

    fn rivian() -> Profile {
        Profile::embedded("rivian_roadcam").unwrap().unwrap()
    }

    #[test]
    fn every_embedded_profile_parses_under_its_own_id() {
        for (id, _) in EMBEDDED {
            let p = Profile::embedded(id).unwrap().unwrap();
            assert_eq!(&p.profile.id, id);
        }
    }

    #[test]
    fn clip_name_is_the_filename_unless_matching_the_path() {
        let parts = ["09_25_26_063343".to_string(), "front.mp4".to_string()];
        assert_eq!(gm().clip_name(&parts).as_deref(), Some("front.mp4"));
        assert_eq!(
            rivian().clip_name(&parts).as_deref(),
            Some("09_25_26_063343__front.mp4")
        );
        assert_eq!(rivian().clip_name(&[]), None);
    }

    #[test]
    fn rivian_takes_the_timestamp_from_the_session_folder() {
        let p = rivian();
        assert_eq!(p.recording.root, "RoadCam/DriveStream");
        assert!(!p.has_rolling_window());
        assert!(gm().has_rolling_window());

        // Month-first folder name with a two-digit year.
        let info = p.parse_clip_filename("09_25_26_063343__front.mp4").unwrap();
        assert_eq!(info.camera, "front");
        assert_eq!(
            info.timestamp.format("%Y-%m-%d_%H-%M-%S").to_string(),
            "2026-09-25_06-33-43"
        );
        assert!(p.parse_clip_filename("11_06_26_150829__Rear_2.MP4").is_some());

        for name in [
            "front.mp4",                       // not inside a session folder
            "09_25_26_063343__notes.txt",      // not a video
            "13_40_26_256161__front.mp4",      // impossible date/time
            "2026_09_25_063343__front.mp4",    // not the session-folder format
        ] {
            assert!(p.parse_clip_filename(name).is_none(), "{name} must be rejected");
        }
    }

    /// The only test that touches the process-wide profile or the override.
    #[test]
    fn reload_switches_the_active_profile_when_the_selection_changes() {
        let dir = std::env::temp_dir().join(format!("dashusb-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profile.toml");
        let select = |id: &str| {
            let toml = EMBEDDED.iter().find(|(pid, _)| *pid == id).unwrap().1;
            std::fs::write(&path, toml).unwrap();
        };
        std::env::set_var("DASHUSB_PROFILE_PATH", &path);

        select("gm_surroundvision");
        Profile::reload();
        let before = Profile::active();
        assert_eq!(before.profile.id, "gm_surroundvision");
        assert!(!Profile::reload(), "an unchanged selection is not a switch");

        select("rivian_roadcam");
        assert!(Profile::reload());
        assert_eq!(Profile::active().profile.id, "rivian_roadcam");
        assert_eq!(before.profile.id, "gm_surroundvision", "old references stay valid");

        std::env::remove_var("DASHUSB_PROFILE_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_marks_only_unconfirmed_profiles_experimental() {
        let all = Profile::all_embedded();
        assert_eq!(all.len(), EMBEDDED.len());
        assert!(!gm().profile.experimental);
        assert!(rivian().profile.experimental);
        assert!((gm().approx_gb_per_hour().unwrap() - 5.3).abs() < 0.1);
        assert_eq!(rivian().approx_gb_per_hour(), None);
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
