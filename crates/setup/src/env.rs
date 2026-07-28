//! Pi environment detection.

use std::fs;
use std::path::Path;

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiModel {
    Pi5,
    Pi4,
    Pi3,
    PiZero2,
    PiZeroW,
    Pi2,
    Rock4CPlus,
    Other,
}

impl PiModel {
    pub fn detect() -> Self {
        let model = fs::read_to_string("/sys/firmware/devicetree/base/model")
            .unwrap_or_default()
            .replace('\0', "");
        let lower = model.to_lowercase();

        // Every match REQUIRES the "raspberry pi" prefix so non-Pi boards
        // whose model string happens to contain "zero" or "pi N" ("Radxa Zero
        // 3W", "Radxa ROCK Pi 4") fall through to Other and take the non-Pi
        // setup paths instead of inheriting Pi-specific config.txt, dwc2, and
        // UDC assumptions.
        if lower.contains("raspberry pi 5") {
            PiModel::Pi5
        } else if lower.contains("raspberry pi 4") {
            PiModel::Pi4
        } else if lower.contains("raspberry pi 3") {
            PiModel::Pi3
        } else if lower.contains("raspberry pi zero 2") {
            PiModel::PiZero2
        } else if lower.contains("raspberry pi zero") {
            PiModel::PiZeroW
        } else if lower.contains("raspberry pi 2") {
            PiModel::Pi2
        } else if lower.contains("rock 4c+")
            || lower.contains("rock-4c-plus")
            || lower.contains("rock pi 4c+")
            || dt_compatible_contains("rock-4c-plus")
        {
            PiModel::Rock4CPlus
        } else {
            PiModel::Other
        }
    }

    /// The config.txt section name for this Pi model's dtoverlay.
    pub fn config_section(&self) -> &'static str {
        match self {
            PiModel::Pi5 => "pi5",
            PiModel::Pi4 => "pi4",
            PiModel::Pi3 => "all", // Pi3 uses global section
            PiModel::PiZero2 => "pi02",
            _ => "all",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            PiModel::Pi5 => "Raspberry Pi 5",
            PiModel::Pi4 => "Raspberry Pi 4",
            PiModel::Pi3 => "Raspberry Pi 3",
            PiModel::PiZero2 => "Raspberry Pi Zero 2 W",
            PiModel::PiZeroW => "Raspberry Pi Zero W",
            PiModel::Pi2 => "Raspberry Pi 2",
            PiModel::Rock4CPlus => "Radxa ROCK 4C+",
            PiModel::Other => "Unknown board",
        }
    }
}

/// True if the device-tree `compatible` string contains `needle`. The model
/// string alone can vary across vendor DTBs (e.g. "Radxa ROCK 4C+" vs
/// "RADXA ROCK 4C Plus"), so `compatible` (e.g. `radxa,rock-4c-plus`) is the
/// reliable fallback for board detection.
fn dt_compatible_contains(needle: &str) -> bool {
    fs::read_to_string("/sys/firmware/devicetree/base/compatible")
        .map(|s| s.replace('\0', " ").to_lowercase().contains(needle))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct SetupEnv {
    pub pi_model: PiModel,
    /// Boot partition (/dashusb -> /boot/firmware or /boot).
    pub boot_path: String,
    /// Path to cmdline.txt if it exists.
    pub cmdline_path: Option<String>,
    /// Path to config.txt if it exists.
    pub piconfig_path: Option<String>,
    /// The boot disk device (e.g. /dev/mmcblk0).
    pub boot_disk: Option<String>,
    /// Root partition device (e.g. /dev/mmcblk0p2).
    pub root_partition: Option<String>,
    /// External data drive set in config, if any.
    pub data_drive: Option<String>,
    /// Parsed configuration values.
    pub config: std::collections::HashMap<String, String>,
}

impl SetupEnv {
    pub async fn detect() -> Result<Self> {
        let pi_model = PiModel::detect();

        ensure_sentryusb_symlink()?;

        let boot_path = fs::read_link("/dashusb")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/boot".to_string());

        // /dashusb first, preserving the user's chosen boot dir, then the
        // canonical locations so a broken symlink left by a prior install
        // cannot make every cmdline/config edit silently no-op. Bookworm puts
        // the boot files under /boot/firmware; older images use /boot.
        let cmdline_path = [
            "/dashusb/cmdline.txt",
            "/boot/firmware/cmdline.txt",
            "/boot/cmdline.txt",
        ]
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|s| s.to_string());

        let piconfig_path = [
            "/dashusb/config.txt",
            "/boot/firmware/config.txt",
            "/boot/config.txt",
        ]
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|s| s.to_string());

        let boot_disk = detect_boot_disk().await.ok();
        let root_partition = detect_root_partition().await.ok();

        // Only *active* (uncommented) exports count. Commented sample lines in
        // dashusb.conf are documentation, not user choices; merging them in
        // would run every optional phase (AP setup, extra drives) against
        // sample defaults the user never picked.
        let config_path = sentryusb_config::find_config_path();
        let mut config = sentryusb_config::parse_file(config_path)
            .map(|(active, _commented)| active)
            .unwrap_or_default();

        // Migrate legacy key names (lowercase and renamed settings). The old
        // value is copied only when the new key is unset, so user edits to the
        // new name always win.
        migrate_legacy_config_keys(&mut config);

        let data_drive = config.get("DATA_DRIVE")
            .filter(|v| !v.is_empty())
            .cloned();

        Ok(SetupEnv {
            pi_model,
            boot_path,
            cmdline_path,
            piconfig_path,
            boot_disk,
            root_partition,
            data_drive,
            config,
        })
    }

    pub fn get(&self, key: &str, default: &str) -> String {
        self.config.get(key).cloned().unwrap_or_else(|| default.to_string())
    }

    /// Only the literals `true` and `false` are recognized; anything else
    /// falls back to `default`.
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.config.get(key).map(|s| s.as_str()) {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        }
    }
}


/// Copy legacy config keys to their current names. The new name wins: a key
/// the user already set is never overwritten from its legacy counterpart.
fn migrate_legacy_config_keys(config: &mut std::collections::HashMap<String, String>) {
    const LEGACY_MAP: &[(&str, &str)] = &[
        ("archiveserver", "ARCHIVE_SERVER"),
        ("camsize", "CAM_SIZE"),
        ("sharename", "SHARE_NAME"),
        ("shareuser", "SHARE_USER"),
        ("sharepassword", "SHARE_PASSWORD"),
        ("timezone", "TIME_ZONE"),
        ("usb_drive", "DATA_DRIVE"),
        ("USB_DRIVE", "DATA_DRIVE"),
        ("archivedelay", "ARCHIVE_DELAY"),
        ("trigger_file_saved", "TRIGGER_FILE_SAVED"),
        ("trigger_file_sentry", "TRIGGER_FILE_SENTRY"),
        ("trigger_file_any", "TRIGGER_FILE_ANY"),
        ("pushover_enabled", "PUSHOVER_ENABLED"),
        ("pushover_user_key", "PUSHOVER_USER_KEY"),
        ("pushover_app_key", "PUSHOVER_APP_KEY"),
        ("gotify_enabled", "GOTIFY_ENABLED"),
        ("gotify_domain", "GOTIFY_DOMAIN"),
        ("gotify_app_token", "GOTIFY_APP_TOKEN"),
        ("gotify_priority", "GOTIFY_PRIORITY"),
        ("ifttt_enabled", "IFTTT_ENABLED"),
        ("ifttt_event_name", "IFTTT_EVENT_NAME"),
        ("ifttt_key", "IFTTT_KEY"),
        ("sns_enabled", "SNS_ENABLED"),
        ("aws_region", "AWS_REGION"),
        ("aws_access_key_id", "AWS_ACCESS_KEY_ID"),
        ("aws_secret_key", "AWS_SECRET_ACCESS_KEY"),
        ("aws_sns_topic_arn", "AWS_SNS_TOPIC_ARN"),
    ];

    for (old, new) in LEGACY_MAP {
        if config.contains_key(*new) {
            continue;
        }
        if let Some(val) = config.get(*old).cloned() {
            config.insert((*new).to_string(), val);
            config.remove(*old);
        }
    }
}

/// Creates /dashusb -> /boot/firmware (or /boot) if it doesn't exist.
fn ensure_sentryusb_symlink() -> Result<()> {
    let link = Path::new("/dashusb");
    if link.is_symlink() || link.exists() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let target = if Path::new("/boot/firmware").exists() {
            "/boot/firmware"
        } else {
            "/boot"
        };
        std::os::unix::fs::symlink(target, "/dashusb")?;
    }

    Ok(())
}

async fn detect_boot_disk() -> Result<String> {
    // `-p` already makes lsblk emit full paths ("/dev/sda"), so "/dev/" MUST
    // NOT be prepended again: "/dev//dev/sda" makes every sfdisk call on it
    // fail silently, cascading into bogus "not last partition" errors during
    // the shrink phase.
    let output = sentryusb_shell::run(
        "lsblk", &["-dpno", "pkname", &detect_mount_source("/dashusb").await?],
    ).await?;
    let dev = output.trim().to_string();
    if dev.is_empty() {
        anyhow::bail!("could not determine boot disk for /dashusb");
    }
    Ok(dev)
}

async fn detect_root_partition() -> Result<String> {
    let output = sentryusb_shell::run("findmnt", &["-n", "-o", "SOURCE", "/"]).await?;
    Ok(output.trim().to_string())
}

async fn detect_mount_source(mountpoint: &str) -> Result<String> {
    let output = sentryusb_shell::run(
        "findmnt", &["-D", "-no", "SOURCE", "--target", mountpoint],
    ).await?;
    Ok(output.trim().to_string())
}
