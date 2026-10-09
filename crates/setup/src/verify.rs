//! Pre-setup checks ordered around state that setup itself creates.
//! Hardware/XFS/config checks run first, UDC after the dwc2 reboot, and disk
//! space after root shrinking. Failures stop before destructive operations.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::env::{PiModel, SetupEnv};
use crate::error::ConfigError;
use crate::SetupEmitter;

/// Minimum usable space on the SD card after the root-partition shrink; the
/// actual footprint is ~8 GB.
const MIN_SD_SPACE_BYTES: u64 = 8 * (1 << 30);

/// Minimum total size of an external USB drive: 59 GiB, i.e. a nominal 64 GB
/// drive.
const MIN_USB_SIZE_BYTES: u64 = 59 * (1 << 30);

/// Check hardware, XFS, and configuration before the dwc2 overlay phase.
pub async fn early_verify(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    // Announce before the slow XFS install and loopback probe.
    emitter.begin_phase("verify", "Verifying configuration");
    check_supported_hardware(env)?;
    check_xfs_support(emitter).await?;
    check_required_config(env)?;
    Ok(())
}

/// Require a UDC after the dwc2 overlay phase and before partitioning.
pub fn verify_udc() -> Result<()> {
    check_udc()
}

/// Check disk space after root shrinking; existing labels take the fast path.
pub async fn verify_disk_space(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    check_available_space(env, emitter).await
}

fn check_supported_hardware(env: &SetupEnv) -> Result<()> {
    // Other boards use separate paths; Pi 2 lacks gadget hardware and armv6
    // has no supported build.
    match env.pi_model {
        PiModel::Pi5 | PiModel::Pi4 | PiModel::Pi3 | PiModel::PiZero2 => {
            Ok(())
        }
        PiModel::PiZeroW => bail!(
            "STOP: unsupported hardware: Raspberry Pi Zero W. \
             DashUSB requires Pi Zero 2 W or newer (Pi 3, Pi 4, Pi 5)."
        ),
        PiModel::Pi2 => bail!(
            "STOP: unsupported hardware: Raspberry Pi 2. \
             (only Pi Zero 2 W, Pi 3, Pi 4, and Pi 5 have the necessary hardware to run DashUSB)"
        ),
        PiModel::Rock4CPlus => Ok(()),
        PiModel::Other => {
            // Separate board-specific setup paths validate these later.
            Ok(())
        }
    }
}

fn check_udc() -> Result<()> {
    let udc_dir = Path::new("/sys/class/udc");
    let count = match std::fs::read_dir(udc_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_symlink()).unwrap_or(false))
            .count(),
        Err(_) => 0,
    };
    if count == 0 {
        let model = std::fs::read_to_string("/sys/firmware/devicetree/base/model")
            .unwrap_or_default()
            .replace('\0', "");
        bail!(
            "STOP: this device ({}) does not have a UDC driver. \
             Check that dtoverlay=dwc2 is in the correct section of config.txt for your Pi model",
            model.trim()
        );
    }
    Ok(())
}

async fn check_xfs_support(emitter: &SetupEmitter) -> Result<()> {
    emitter.progress("Checking XFS support");

    // Log the potentially slow xfsprogs installation.
    if sentryusb_shell::run("which", &["mkfs.xfs"]).await.is_err() {
        emitter.progress("Installing xfsprogs (this can take 30-60 seconds)...");
        crate::apt::apt_install(
            |m| emitter.progress(m),
            &["xfsprogs"],
            Duration::from_secs(180),
        ).await.context("failed to install xfsprogs")?;
        emitter.progress("xfsprogs installed");
    }

    let img = "/tmp/xfs.img";
    let mnt = "/tmp/xfsmnt";

    // Clear interrupted probes so a busy mount is not misreported as no XFS.
    let _ = sentryusb_shell::run("umount", &[mnt]).await;
    if sentryusb_shell::run("findmnt", &[mnt]).await.is_ok() {
        let _ = sentryusb_shell::run("umount", &["-l", mnt]).await;
        if sentryusb_shell::run("findmnt", &[mnt]).await.is_ok() {
            bail!(
                "STOP: {} is still a mount point after umount + lazy umount — reboot and re-run setup",
                mnt
            );
        }
    }
    let _ = std::fs::remove_file(img);
    let _ = std::fs::remove_dir_all(mnt);

    // 1 GB sparse loopback image; the truncate is metadata-only, near-instant.
    emitter.progress("Creating test XFS image");
    sentryusb_shell::run_with_timeout(
        Duration::from_secs(30),
        "truncate",
        &["-s", "1GB", img],
    )
    .await
    .context("truncate xfs test image")?;

    // The snapshot implementation requires XFS reflink support.
    emitter.progress("Formatting test image with XFS (reflink=1)");
    sentryusb_shell::run_with_timeout(
        Duration::from_secs(30),
        "mkfs.xfs",
        &["-m", "reflink=1", "-f", img],
    )
    .await
    .context("mkfs.xfs failed — kernel likely lacks reflink support")?;

    emitter.progress("Mounting test image");
    std::fs::create_dir_all(mnt)?;
    if sentryusb_shell::run("mount", &[img, mnt]).await.is_err() {
        let _ = std::fs::remove_file(img);
        let _ = std::fs::remove_dir_all(mnt);
        bail!("STOP: xfs does not support required features");
    }

    let _ = sentryusb_shell::run("umount", &[mnt]).await;
    let _ = std::fs::remove_file(img);
    let _ = std::fs::remove_dir_all(mnt);

    emitter.progress("XFS supported");
    Ok(())
}

fn check_required_config(env: &SetupEnv) -> Result<()> {
    // An explicitly empty or literal-0 CAM_SIZE still runs setup (zero
    // triggers the SD fallback); a missing key is a user-config error.
    if !env.config.contains_key("CAM_SIZE") {
        // A missing key fails identically on every retry, so classify it as
        // ConfigError to halt the boot-loop auto-resume and surface it.
        return Err(ConfigError(
            "STOP: Define the variable CAM_SIZE in dashusb.conf like this: \
             export CAM_SIZE=64G (GM requires a 64 GB or larger drive)"
                .into(),
        )
        .into());
    }
    Ok(())
}

async fn check_available_space(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    match env.data_drive.as_deref() {
        None => {
            emitter.progress("DATA_DRIVE is not set. SD card will be used.");
            check_available_space_sd(env, emitter).await
        }
        Some(drive) if Path::new(drive).exists() => {
            emitter.progress(&format!(
                "DATA_DRIVE is set to {}. This will be used for /mutable and /backingfiles.",
                drive
            ));
            check_available_space_usb(drive, emitter).await
        }
        // Missing external media is transient because enumeration can lag a
        // reboot; auto-resume retries it.
        Some(drive) => bail!(
            "STOP: DATA_DRIVE is set to {}, which does not exist.",
            drive
        ),
    }
}

async fn check_available_space_sd(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    emitter.progress("Verifying that there is sufficient space available on the MicroSD card...");

    // Fast path: partitions already exist from a previous run.
    let backingfiles_dev = "/dev/disk/by-label/backingfiles";
    let mutable_dev = "/dev/disk/by-label/mutable";
    if Path::new(backingfiles_dev).exists() && Path::new(mutable_dev).exists() {
        let size_output = sentryusb_shell::run(
            "blockdev",
            &["--getsize64", backingfiles_dev],
        )
        .await
        .context("blockdev --getsize64 backingfiles")?;
        let size: u64 = size_output.trim().parse().unwrap_or(0);
        if size < MIN_SD_SPACE_BYTES {
            bail!(
                "STOP: Existing backingfiles partition is too small ({}GB, need at least {}GB)",
                size / 1024 / 1024 / 1024,
                MIN_SD_SPACE_BYTES / 1024 / 1024 / 1024
            );
        }
        emitter.progress("There is sufficient space available.");
        return Ok(());
    }

    // Fresh partition: `sfdisk -F <disk>` reports free space, with the byte
    // count on the first line of the report.
    let boot_disk = env
        .boot_disk
        .as_deref()
        .context("check_available_space_sd: BOOT_DISK is not set")?;

    let sfdisk_out =
        sentryusb_shell::run("sfdisk", &["-F", boot_disk])
            .await
            .context("sfdisk -F")?;

    // First "N bytes" match wins.
    let available_space = sfdisk_out
        .lines()
        .find_map(parse_bytes_from_line)
        .unwrap_or(0);

    if available_space < MIN_SD_SPACE_BYTES {
        let parted = sentryusb_shell::run("parted", &[boot_disk, "print"])
            .await
            .unwrap_or_default();
        bail!(
            "STOP: The MicroSD card is too small: {}GB available, need at least {}GB.\n{}",
            available_space / 1024 / 1024 / 1024,
            MIN_SD_SPACE_BYTES / 1024 / 1024 / 1024,
            parted
        );
    }

    emitter.progress("There is sufficient space available.");
    Ok(())
}

async fn check_available_space_usb(drive: &str, emitter: &SetupEmitter) -> Result<()> {
    emitter.progress("Verifying that there is sufficient space available on the USB drive ...");

    // Bound lsblk for sleeping or failing USB media.
    let lsblk_out = sentryusb_shell::run_with_timeout(
        Duration::from_secs(30),
        "lsblk",
        &["-pno", "TYPE", drive],
    )
    .await
    .with_context(|| {
        format!(
            "Could not read {} (drive may be unresponsive or disconnected). \
             Try unplugging and reconnecting it.",
            drive
        )
    })?;

    let drive_type = lsblk_out.lines().next().unwrap_or("").trim();
    if drive_type != "disk" {
        bail!(
            "STOP: The specified drive ({}) is not a disk (TYPE={}). \
             Please specify path to the disk.",
            drive,
            drive_type
        );
    }

    let size_out = sentryusb_shell::run_with_timeout(
        Duration::from_secs(30),
        "blockdev",
        &["--getsize64", drive],
    )
    .await
    .with_context(|| {
        format!(
            "Could not read size of {} (drive may be unresponsive). \
             Try unplugging and reconnecting it.",
            drive
        )
    })?;

    let drive_size: u64 = size_out.trim().parse().unwrap_or(0);
    if drive_size < MIN_USB_SIZE_BYTES {
        let parted = sentryusb_shell::run("parted", &[drive, "print"])
            .await
            .unwrap_or_default();
        bail!(
            "STOP: The USB drive is too small: {}GB available. Expected at least 64GB\n{}",
            drive_size / 1024 / 1024 / 1024,
            parted
        );
    }

    emitter.progress("There is sufficient space available.");
    Ok(())
}

/// Parse the first "N bytes" occurrence on a line, e.g.
/// `Unpartitioned space /dev/mmcblk0: 10737418240 bytes, 10.7 GiB`.
fn parse_bytes_from_line(line: &str) -> Option<u64> {
    let bytes_idx = line.find(" bytes")?;
    let prefix = &line[..bytes_idx];
    let digits: String = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{PiModel, SetupEnv};
    use crate::error::ConfigError;
    use crate::SetupEmitter;
    use std::collections::HashMap;

    fn env_with(pairs: &[(&str, &str)]) -> SetupEnv {
        let config: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        SetupEnv {
            pi_model: PiModel::Other,
            cmdline_path: None,
            piconfig_path: None,
            boot_disk: None,
            root_partition: None,
            data_drive: None,
            config,
        }
    }

    #[test]
    fn missing_cam_size_is_a_config_error() {
        // A missing required key fails identically on every retry, so it must
        // classify as ConfigError, which stops the setup boot-loop
        // auto-resume, rather than as a transient failure.
        let env = env_with(&[]);
        let err = check_required_config(&env).unwrap_err();
        assert!(
            err.downcast_ref::<ConfigError>().is_some(),
            "missing CAM_SIZE must be a ConfigError, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn nonexistent_data_drive_stays_transient() {
        // A missing DATA_DRIVE must NOT be a ConfigError. `env.data_drive` is
        // the raw config value with no existence check (env.rs) and this is
        // the first existence gate, so a USB/SSD that is merely slow to
        // enumerate, or not back yet after a mid-setup reboot, must
        // auto-resume and retry rather than halt setup as a config error.
        let mut env = env_with(&[]);
        env.data_drive = Some("/no/such/dashusb/drive".to_string());
        let emitter = SetupEmitter::new(|_| {}, |_, _| {});
        let err = check_available_space(&env, &emitter).await.unwrap_err();
        assert!(
            err.downcast_ref::<ConfigError>().is_none(),
            "nonexistent DATA_DRIVE must stay transient (self-heals on retry), got ConfigError: {err:?}"
        );
    }

    #[test]
    fn parse_bytes_picks_trailing_number_before_bytes() {
        // Real sfdisk output shape.
        let line = "Unpartitioned space /dev/mmcblk0: 10737418240 bytes, 10.7 GiB";
        assert_eq!(parse_bytes_from_line(line), Some(10_737_418_240));
    }

    #[test]
    fn parse_bytes_none_when_absent() {
        // " bytes" matches but no digits immediately before it.
        assert_eq!(parse_bytes_from_line("no bytes here"), None);
        assert_eq!(
            parse_bytes_from_line("/dev/mmcblk0 30GB"),
            None,
            "no `bytes` substring → no match"
        );
    }

    #[test]
    fn parse_bytes_handles_leading_text() {
        assert_eq!(
            parse_bytes_from_line("size: 123456789 bytes total"),
            Some(123_456_789)
        );
    }
}
