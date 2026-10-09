//! FAT32 camera-disk image creation under `/backingfiles`.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::env::SetupEnv;
use crate::SetupEmitter;

const BACKINGFILES: &str = "/backingfiles";

struct DriveSpec {
    name: &'static str,
    config_key: &'static str,
}

const DRIVE_SPECS: &[DriveSpec] = &[
    DriveSpec { name: "cam", config_key: "CAM_SIZE" },
];

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
    // Use available space because snapshots share this filesystem. The caller
    // separately credits images that will be recreated.
    let output = sentryusb_shell::run(
        "df", &["--output=avail", "--block-size=1K", &format!("{}/", BACKINGFILES)],
    ).await?;
    let avail: u64 = output.lines().last().unwrap_or("0").trim().parse().unwrap_or(0);

    // Keep a reserve so image creation can never squeeze the fs to zero:
    // 10% capped between 2GB and 10GB.
    let ten_pct = avail / 10;
    let min_pad = 2 * 1024 * 1024;
    let max_pad = 10 * 1024 * 1024;
    let padding = ten_pct.max(min_pad).min(max_pad);
    Ok(avail.saturating_sub(padding))
}

/// Match size within 10 MiB and require the expected FAT32 partition type.
fn image_matches(file: &str, requested_kb: u64) -> bool {
    if requested_kb == 0 {
        return !Path::new(file).exists();
    }
    if let Ok(meta) = std::fs::metadata(file) {
        let current_kb = meta.len() / 1024;
        let diff = (current_kb as i64 - requested_kb as i64).unsigned_abs();
        if diff >= 10240 {
            return false;
        }
        // MBR partition type at byte 450 (446 + 4, the type field of
        // partition entry 1): 0x0c = FAT32 LBA, the type setup creates;
        // 0x07 = exFAT/NTFS from a legacy USE_EXFAT image.
        if let Ok(mut f) = std::fs::File::open(file) {
            use std::io::{Read, Seek, SeekFrom};
            let mut b = [0u8; 1];
            if f.seek(SeekFrom::Start(450)).is_ok() && f.read_exact(&mut b).is_ok() {
                return b[0] == 0x0c;
            }
        }
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

    // Never silently format a profile requesting an unsupported filesystem.
    let fs = &sentryusb_vehicle_profile::Profile::active().virtual_drive.filesystem;
    if !fs.eq_ignore_ascii_case("fat32") {
        anyhow::bail!("vehicle profile requests unsupported filesystem {fs:?}; only fat32 is implemented");
    }
    sentryusb_shell::run(
        "bash", &["-c", &format!("echo 'type=c' | sfdisk '{}'", filename)],
    ).await.context("sfdisk failed on disk image")?;

    let offset = get_partition_offset(&filename).await?;

    let loopdev = sentryusb_shell::run(
        "losetup", &["-f", "--show", "-o", &offset.to_string(), &filename],
    ).await.context("losetup failed")?.trim().to_string();

    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=5"]).await;

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
    let _ = sentryusb_gadget::disable();
    let _ = sentryusb_shell::run("umount", &["-d", "/mnt/cam"]).await;
    let _ = sentryusb_shell::run(
        "bash", &["-c", "umount -d /backingfiles/snapshots/snap*/mnt 2>/dev/null"],
    ).await;
}

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

/// Create all disk images from the config settings. Returns true if any work
/// was performed.
pub async fn create_disk_images(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    let profile = sentryusb_vehicle_profile::Profile::active();
    let min_kb = dehumanize(&profile.virtual_drive.min_size)?;

    // Resolve profile sizes first so matching images short-circuit all work.
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
                "CAM_SIZE {} is below the vehicle profile minimum {}. The car requires a drive of at least that size to record.",
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

    if sizes.iter().all(|(_, _, sz, f)| image_matches(f, *sz)) {
        return Ok(false);
    }

    emitter.begin_phase("disk_images", "Disk images");
    emitter.progress("Creating disk images...");

    ensure_vfat_tools(emitter).await?;

    // Reject insufficient space; never shrink images or delete snapshots.
    // Sparse rolling-only images reserve:
    //   min(logical, max(0.5 * logical, 2 * rolling window for live plus one
    //   COW generation)) + snapshot reserve
    // Keep snapshot reserve outside the cap and a 0.5x logical floor for
    // segment-size variance.
    let logical_requested: u64 = sizes.iter().map(|(_, _, sz, _)| sz).sum();
    let total_requested: u64 = if profile.features.event_folders {
        logical_requested
    } else {
        let seg = profile.recording.segment_seconds.max(1) as u64;
        let window_secs = profile.recording.rolling_window_minutes as u64 * 60;
        let rolling_bytes = profile.recording.approx_bytes_per_camera_segment
            * profile.cameras.len() as u64
            * window_secs.div_ceil(seg);
        let rolling_kb = rolling_bytes / 1024;
        const SNAPSHOT_RESERVE_KB: u64 = 15 * 1024 * 1024; // 15 GB
        logical_requested
            .min((logical_requested / 2).max(rolling_kb.saturating_mul(2)))
            .saturating_add(SNAPSHOT_RESERVE_KB)
    };
    // Credit back the allocation of images about to be deleted and recreated.
    // du reports referenced KB and reflink-shared snapshot blocks stay
    // allocated either way, so this only ever under-frees.
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

    release_all_images().await;

    let cam_changed = !image_matches(&sizes[0].3, sizes[0].2);
    for (name, label, size_kb, filename) in &sizes {
        if image_matches(filename, *size_kb) {
            continue;
        }
        emitter.progress(&format!("Recreating {} drive ({}K)...", name, size_kb));
        create_drive(name, label, *size_kb, emitter).await?;
    }

    // Rebuilt camera images invalidate recording links. Never remove snapshots,
    // which hold independent archived footage.
    if sizes[0].2 == 0 || cam_changed {
        if Path::new("/mutable/Recordings").is_dir() {
            let _ = std::fs::remove_dir_all("/mutable/Recordings/Continuous");
            let _ = std::fs::remove_file("/mutable/recordings_archived");
        }
    }

    emitter.progress("Disk image creation complete.");
    Ok(true)
}
