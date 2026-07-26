//! Disk image creation — replaces `create-backingfiles.sh`.
//!
//! Creates the FAT32 cam disk image
//! drives in /backingfiles/. Wraps & License Plates live as folders on the
//! cam drive — no dedicated partition.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::env::SetupEnv;
use crate::SetupEmitter;

const BACKINGFILES: &str = "/backingfiles";

/// Disk image spec.
struct DriveSpec {
    name: &'static str,
    config_key: &'static str,
}

const DRIVE_SPECS: &[DriveSpec] = &[
    DriveSpec { name: "cam", config_key: "CAM_SIZE" },
];

/// One-time cleanup for installs that previously had a dedicated wraps disk.
/// The 4 GB image is no longer used — Wraps & LicensePlate are now folders
/// on the cam drive. Reclaim the space on the next setup re-run.
fn purge_legacy_wraps_disk() {
    let _ = std::fs::remove_file(format!("{}/wraps_disk.bin", BACKINGFILES));
    let _ = std::fs::remove_file(format!("{}/wraps_disk.bin.opts", BACKINGFILES));
    let _ = std::fs::remove_dir("/mnt/wraps");
}

/// Parse a human-readable size like "30G", "4G", "100M" into KB.
pub fn dehumanize(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase()
        .replace("GB", "G")
        .replace("MB", "M")
        .replace("KB", "K");

    if s == "0" || s.is_empty() {
        return Ok(0);
    }

    if s.ends_with('G') {
        let n: f64 = s.trim_end_matches('G').parse()?;
        Ok((n * 1024.0 * 1024.0) as u64) // KB
    } else if s.ends_with('M') {
        let n: f64 = s.trim_end_matches('M').parse()?;
        Ok((n * 1024.0) as u64)
    } else if s.ends_with('K') {
        let n: f64 = s.trim_end_matches('K').parse()?;
        Ok(n as u64)
    } else {
        // Assume bytes
        let n: u64 = s.parse()?;
        Ok(n / 1024)
    }
}

/// Get available space in KB on /backingfiles, minus a safety margin.
async fn available_space_kb() -> Result<u64> {
    // AVAILABLE space, not filesystem size — with snapshots living on
    // the same filesystem, sizing against the total let a re-run pass
    // the check and then fail at truncate once snapshots had grown.
    // Images being recreated are deleted first, so their current
    // allocation is credited back by the caller.
    let output = sentryusb_shell::run(
        "df", &["--output=avail", "--block-size=1K", &format!("{}/", BACKINGFILES)],
    ).await?;
    let avail: u64 = output.lines().last().unwrap_or("0").trim().parse().unwrap_or(0);

    // Keep a reserve so image creation can never squeeze the fs to zero:
    // 10% capped between 2GB and 10GB.
    let ten_pct = avail / 10;
    let min_pad = 2 * 1024 * 1024; // 2GB in KB
    let max_pad = 10 * 1024 * 1024; // 10GB in KB
    let padding = ten_pct.max(min_pad).min(max_pad);
    Ok(avail.saturating_sub(padding))
}

/// Check if an existing image file matches the requested size (within 10MB).
fn image_matches(file: &str, requested_kb: u64) -> bool {
    if requested_kb == 0 {
        return !Path::new(file).exists();
    }
    if let Ok(meta) = std::fs::metadata(file) {
        let current_kb = meta.len() / 1024;
        let diff = (current_kb as i64 - requested_kb as i64).unsigned_abs();
        diff < 10240
    } else {
        false
    }
}

/// Create a single drive image file with a partition table and filesystem.
async fn create_drive(
    name: &str,
    label: &str,
    size_kb: u64,
    emitter: &SetupEmitter,
) -> Result<()> {
    let filename = format!("{}/{}_disk.bin", BACKINGFILES, name);
    let mountpoint = format!("/mnt/{}", name);

    if size_kb == 0 {
        let _ = std::fs::remove_file(&filename);
        let _ = std::fs::remove_file(format!("{}.opts", filename));
        let _ = std::fs::remove_dir(&mountpoint);
        return Ok(());
    }

    emitter.progress(&format!("Allocating {}K for {}...", size_kb, filename));
    let _ = std::fs::remove_file(&filename);
    sentryusb_shell::run("truncate", &["--size", &format!("{}K", size_kb), &filename]).await
        .context("truncate failed")?;

    // Create partition table. FAT32 always — the GM head unit requires
    // it, and the vehicle profile pins `filesystem = "fat32"`.
    sentryusb_shell::run(
        "bash", &["-c", &format!("echo 'type=c' | sfdisk '{}'", filename)],
    ).await.context("sfdisk failed on disk image")?;

    // Find partition offset
    let offset = get_partition_offset(&filename).await?;

    // Set up loop device
    let loopdev = sentryusb_shell::run(
        "losetup", &["-f", "--show", "-o", &offset.to_string(), &filename],
    ).await.context("losetup failed")?.trim().to_string();

    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=5"]).await;

    // Format
    emitter.progress(&format!("Creating filesystem with label '{}'", label));
    let format_result =
        sentryusb_shell::run("mkfs.vfat", &[&loopdev, "-F", "32", "-n", label]).await;

    let _ = sentryusb_shell::run("losetup", &["-d", &loopdev]).await;
    format_result.context("filesystem creation failed")?;

    let _ = std::fs::create_dir_all(&mountpoint);
    emitter.progress(&format!("Drive image {} ready.", filename));
    Ok(())
}

/// Get the byte offset of the first partition in a disk image.
async fn get_partition_offset(filename: &str) -> Result<u64> {
    let bytes_out = sentryusb_shell::run(
        "bash", &["-c", &format!("sfdisk -l -o Size -q --bytes '{}' | tail -1", filename)],
    ).await?;
    let size_in_bytes: u64 = bytes_out.trim().parse().context("parse size")?;

    let sectors_out = sentryusb_shell::run(
        "bash", &["-c", &format!("sfdisk -l -o Sectors -q '{}' | tail -1", filename)],
    ).await?;
    let size_in_sectors: u64 = sectors_out.trim().parse().context("parse sectors")?;

    let sector_size = size_in_bytes / size_in_sectors;

    let start_out = sentryusb_shell::run(
        "bash", &["-c", &format!("sfdisk -l -o Start -q '{}' | tail -1", filename)],
    ).await?;
    let start_sector: u64 = start_out.trim().parse().context("parse start")?;

    Ok(start_sector * sector_size)
}

/// Release all loop devices and unmount all drive image mount points.
async fn release_all_images() {
    let _ = sentryusb_shell::run("bash", &["-c", "killall archiveloop 2>/dev/null"]).await;
    // Use the usb_gadget crate to disable
    let _ = sentryusb_gadget::disable();
    // /mnt/wraps stays in the list to drain any leftover mount from a
    // pre-migration install before purge_legacy_wraps_disk runs.
    for mount in &["/mnt/cam", "/mnt/wraps"] {
        let _ = sentryusb_shell::run("umount", &["-d", mount]).await;
    }
    let _ = sentryusb_shell::run(
        "bash", &["-c", "umount -d /backingfiles/snapshots/snap*/mnt 2>/dev/null"],
    ).await;
}

/// Ensure dosfstools is available.
async fn ensure_vfat_tools(emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("which", &["mkfs.vfat"]).await.is_err() {
        crate::apt::apt_install(
            |m| emitter.progress(m),
            &["dosfstools"],
            Duration::from_secs(600),
        ).await.context("failed to install dosfstools")?;
    }
    Ok(())
}

/// Create all disk images based on config settings. Returns true if any work was performed.
pub async fn create_disk_images(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    let profile = sentryusb_vehicle_profile::Profile::active();
    let min_kb = dehumanize(&profile.virtual_drive.min_size)?;

    // Calculate requested sizes first (before any heavy work) so we can
    // short-circuit when everything already matches. Size defaults, the
    // volume label, and the size floor all come from the vehicle profile.
    let mut sizes: Vec<(String, String, u64, String)> = Vec::new();
    for spec in DRIVE_SPECS {
        let raw = env.get(spec.config_key, &profile.virtual_drive.default_size);
        let size_kb = if raw.contains('%') {
            dehumanize(&profile.virtual_drive.default_size)?
        } else {
            dehumanize(&raw)?
        };
        if size_kb < min_kb {
            bail!(
                "CAM_SIZE {} is below the vehicle profile minimum {} — the                  car requires a drive of at least that size to record.",
                raw, profile.virtual_drive.min_size,
            );
        }
        let filename = format!("{}/{}_disk.bin", BACKINGFILES, spec.name);
        sizes.push((
            spec.name.to_string(),
            profile.virtual_drive.label.clone(),
            size_kb,
            filename,
        ));
    }

    // Reclaim the 4 GB the dedicated wraps disk used to occupy. Runs before
    // the all-match early exit so a pre-migration install gets cleaned up
    // even when the user hasn't changed any sizes.
    let legacy_wraps_path = format!("{}/wraps_disk.bin", BACKINGFILES);
    let legacy_wraps = Path::new(&legacy_wraps_path).exists();
    if legacy_wraps {
        emitter.progress("Removing legacy wraps disk image — using cam drive folders now...");
        let _ = sentryusb_shell::run("umount", &["-d", "/mnt/wraps"]).await;
        purge_legacy_wraps_disk();
    }

    let all_match = sizes.iter().all(|(_, _, sz, f)| image_matches(f, *sz));
    if all_match && !legacy_wraps {
        return Ok(false);
    }

    emitter.begin_phase("disk_images", "Disk images");
    emitter.progress("Creating disk images...");

    ensure_vfat_tools(emitter).await?;

    // Space check. teslausb auto-shrinks because it has no UI to ask
    // the user; we have a UI, so we reject explicitly with a clear
    // breakdown. The wizard pre-flight surfaces the same calculation
    // before submit (see verify::verify_disk_space). Never auto-delete
    // snapshots as a side effect of a settings change.
    let total_requested: u64 = sizes.iter().map(|(_, _, sz, _)| sz).sum();
    // Credit back the allocation of images we're about to delete and
    // recreate (du reports referenced KB; reflink-shared snapshot blocks
    // stay allocated either way, so this only ever under-frees).
    let mut reclaimable: u64 = 0;
    for (_, _, sz, filename) in &sizes {
        if Path::new(filename).exists() && !image_matches(filename, *sz) {
            if let Ok(out) =
                sentryusb_shell::run("du", &["--block-size=1K", filename]).await
            {
                reclaimable += out
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
            }
        }
    }
    let available = available_space_kb().await.unwrap_or(0).saturating_add(reclaimable);
    if total_requested > available {
        let need_gb = (total_requested - available) / 1024 / 1024;
        let req_gb = total_requested / 1024 / 1024;
        let avail_gb = available / 1024 / 1024;
        bail!(
            "Disk images need {} GB but backingfiles has only {} GB free \
             (after safety reserve). Free at least {} GB by deleting \
             snapshots from the snapshot management page, then re-run setup.",
            req_gb, avail_gb, need_gb,
        );
    }

    // Release everything that might be using the images
    release_all_images().await;

    // Create/update each drive
    let cam_changed = !image_matches(&sizes[0].3, sizes[0].2);
    for (name, label, size_kb, filename) in &sizes {
        if image_matches(filename, *size_kb) {
            continue;
        }
        emitter.progress(&format!("Recreating {} drive ({}K)...", name, size_kb));
        create_drive(name, label, *size_kb, emitter).await?;
    }

    // Clean up stale /mutable/Recordings symlinks when cam drive was
    // changed/removed — those symlinks point into the old cam_disk and
    // are dangling after the recreate. Snapshots are intentionally NOT
    // touched: they live independently on backingfiles and represent
    // the user's archived footage history. Wiping them on a CAM_SIZE
    // change is the same "I changed a setting, why did I lose data"
    // failure mode the partition wipe used to cause.
    if sizes[0].2 == 0 || cam_changed {
        if Path::new("/mutable/Recordings").is_dir() {
            let _ = std::fs::remove_dir_all("/mutable/Recordings/Continuous");
            let _ = std::fs::remove_file("/mutable/recordings_archived");
        }
    }

    emitter.progress("Disk image creation complete.");
    Ok(true)
}
