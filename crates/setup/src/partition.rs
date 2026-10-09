//! Safe provisioning of backingfiles (XFS) and mutable (ext4) partitions.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::info;

use crate::env::SetupEnv;
use crate::SetupEmitter;

const BACKINGFILES_MOUNT: &str = "/backingfiles";
const MUTABLE_MOUNT: &str = "/mutable";

/// Probe for both label symlinks. Can momentarily return false on a udev race
/// even when the partitions are fine, so never treat false as "safe to wipe".
pub async fn partitions_exist() -> bool {
    Path::new("/dev/disk/by-label/backingfiles").exists()
        && Path::new("/dev/disk/by-label/mutable").exists()
}

async fn ensure_xfs_tools(emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("which", &["mkfs.xfs"]).await.is_err() {
        info!("Installing xfsprogs...");
        emitter.progress("Installing xfsprogs...");
        crate::apt::apt_install(
            |m| emitter.progress(m),
            &["xfsprogs"],
            Duration::from_secs(600),
        ).await.context("failed to install xfsprogs")?;
    }
    Ok(())
}

/// "p" for mmcblk/nvme/loop devices, "" for sd-style devices.
fn partition_prefix(device: &str) -> &'static str {
    if device.contains("mmcblk") || device.contains("nvme") || device.contains("loop") {
        "p"
    } else {
        ""
    }
}

/// Create partitions on an external DATA_DRIVE. Returns true if any work was
/// performed.
pub async fn setup_data_drive(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    let data_drive = env.data_drive.as_deref()
        .context("DATA_DRIVE not set")?;

    let prefix = partition_prefix(data_drive);
    let p1 = format!("{}{}{}", data_drive, prefix, 1);
    let p2 = format!("{}{}{}", data_drive, prefix, 2);

    // Refuse a new DATA_DRIVE while old labeled partitions remain attached;
    // duplicate labels make mounts ambiguous and risk wiping the wrong disk.
    if let Some(stale) = label_on_other_drive(data_drive).await {
        bail!(
            "DATA_DRIVE is set to {} but the {} from a previous setup is still \
             attached at {}. Disconnect the old drive before re-running setup, \
             or change DATA_DRIVE back to {}. Your old drive will not be modified.",
            data_drive, stale.label, stale.device, stale.parent
        );
    }

    // FINISHED is a second guard against a transient udev miss entering the
    // destructive path. Manual marker removal is the explicit wipe opt-in.
    let setup_finished = std::path::Path::new("/dashusb/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/firmware/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/DASHUSB_SETUP_FINISHED").exists();

    let bf_ok = check_label_matches(&p2, "backingfiles").await;
    let mut_ok = check_label_matches(&p1, "mutable").await;
    let bf_xfs = check_fstype(&p2, "xfs").await;
    let mut_ext4 = check_fstype(&p1, "ext4").await;

    let already_partitioned = bf_ok && mut_ok && bf_xfs && mut_ext4;

    // Correct labels/filesystems are authoritative; repair fstab without
    // touching data.
    if already_partitioned {
        emitter.progress(&format!(
            "Existing backingfiles (xfs) and mutable (ext4) partitions found on {}. Keeping them.",
            data_drive
        ));
        // Quiesce holders, but never repair or format an established drive.
        let _ = sentryusb_shell::run("bash", &["-c", "killall archiveloop 2>/dev/null"]).await;
        let _ = sentryusb_gadget::disable();
        let _ = sentryusb_shell::run(
            "bash",
            &["-c",
              "for loop in $(losetup -a 2>/dev/null | grep -E '/backingfiles/|/mnt/' | cut -d: -f1); do \
                 umount \"$loop\" 2>/dev/null; losetup -d \"$loop\" 2>/dev/null; \
               done"],
        ).await;
        cleanup_mounts().await;
        let _ = sentryusb_shell::run("umount", &[p1.as_str()]).await;
        let _ = sentryusb_shell::run("umount", &[p2.as_str()]).await;
        tokio::time::sleep(Duration::from_secs(2)).await;

        update_fstab().await?;
        return Ok(false);
    }

    // Never enter wipefs/parted/mkfs after setup has finished.
    if setup_finished {
        bail!(
            "Refusing to wipe {}: setup previously completed on this device, \
             but the partition labels or filesystem types are not what we \
             expected ({} backingfiles label match, {} mutable label match, \
             {} backingfiles is xfs, {} mutable is ext4). The drive contents \
             have NOT been modified. If the drive really needs to be \
             reformatted, delete /dashusb/DASHUSB_SETUP_FINISHED and \
             re-run setup. Otherwise, reboot to let udev resettle and try again.",
            data_drive,
            if bf_ok { "✓" } else { "✗" },
            if mut_ok { "✓" } else { "✗" },
            if bf_xfs { "✓" } else { "✗" },
            if mut_ext4 { "✓" } else { "✗" },
        );
    }

    emitter.begin_phase("partitions", "Disk partitioning");
    emitter.progress(&format!("DATA_DRIVE is set to {}", data_drive));
    emitter.progress(&format!("Unmounting partitions on {}...", data_drive));
    cleanup_mounts().await;

    // Release automounts and loops outside the known managed paths.
    emitter.progress(&format!("Releasing kernel-side holders on {}...", data_drive));
    release_data_drive(data_drive, emitter).await;

    emitter.progress(&format!("WARNING: This will delete EVERYTHING on {}", data_drive));
    // Bound operations so a wedged USB bridge cannot hang setup indefinitely.
    let op_timeout = Duration::from_secs(120);
    sentryusb_shell::run_with_timeout(op_timeout, "wipefs", &["-afq", data_drive]).await
        .context("wipefs failed (drive unresponsive?)")?;
    sentryusb_shell::run_with_timeout(op_timeout, "parted",
        &[data_drive, "--script", "mktable", "gpt"]).await
        .context("parted mktable failed")?;

    emitter.progress("Creating partitions...");
    sentryusb_shell::run_with_timeout(op_timeout, "parted",
        &["-a", "optimal", "-m", data_drive, "mkpart", "primary", "ext4", "0%", "2GB"]).await?;
    sentryusb_shell::run_with_timeout(op_timeout, "parted",
        &["-a", "optimal", "-m", data_drive, "mkpart", "primary", "ext4", "2GB", "100%"]).await?;

    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=30"]).await;

    emitter.progress(&format!("Formatting mutable partition (ext4) on {}...", p1));
    sentryusb_shell::run_with_timeout(op_timeout, "mkfs.ext4",
        &["-F", "-L", "mutable", &p1]).await.context("mkfs.ext4 failed")?;

    emitter.progress(&format!("Formatting backingfiles partition (xfs) on {}...", p2));
    // Skip full-device TRIM on a fresh partition.
    sentryusb_shell::run_with_timeout(op_timeout, "mkfs.xfs",
        &["-f", "-K", "-m", "reflink=1", "-L", "backingfiles", &p2]).await.context("mkfs.xfs failed")?;

    emitter.progress("Partition formatting complete.");

    update_fstab().await?;
    Ok(true)
}

/// Create partitions on the SD card after the root partition. Returns true if
/// work was done.
pub async fn setup_sd_card(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    let boot_disk = env.boot_disk.as_deref()
        .context("Could not detect boot disk")?;

    // Repair fstab without touching correct partitions.
    if partitions_exist().await {
        update_fstab().await?;
        return Ok(false);
    }

    // Never run sfdisk against a completed installation.
    let setup_finished = std::path::Path::new("/dashusb/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/firmware/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/DASHUSB_SETUP_FINISHED").exists();
    if setup_finished {
        bail!(
            "Refusing to repartition the SD card: setup previously completed \
             but partitions_exist() returned false (label symlinks may have \
             temporarily disappeared due to a udev race). Reboot and try \
             again, or delete /dashusb/DASHUSB_SETUP_FINISHED to force \
             a fresh install."
        );
    }

    emitter.begin_phase("partitions", "Disk partitioning");

    ensure_xfs_tools(emitter).await?;

    emitter.progress("Creating backingfiles and mutable partitions on SD card...");

    let output = sentryusb_shell::run(
        "bash", &["-c", &format!(
            "sfdisk -q -l {} | tail +2 | sort -n -k 2 | tail -1 | awk '{{print $1}}'", boot_disk
        )],
    ).await?;
    let last_part_dev = output.trim().to_string();
    let last_part_num: u32 = last_part_dev.chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .context("could not parse partition number")?;

    let prefix = partition_prefix(boot_disk);
    let bf_dev = format!("{}{}{}", boot_disk, prefix, last_part_num + 1);
    let mut_dev = format!("{}{}{}", boot_disk, prefix, last_part_num + 2);

    let disk_sectors: u64 = sentryusb_shell::run(
        "blockdev", &["--getsz", boot_disk],
    ).await?.trim().parse().context("blockdev parse error")?;

    let last_disk_sector = disk_sectors - 1;
    // Reserve the trailing 300 MB (614400 sectors) for mutable.
    let first_mutable_sector = last_disk_sector - 614400 + 1;

    let last_part_end: u64 = sentryusb_shell::run(
        "bash", &["-c", &format!(
            "sfdisk -o End -q -l {} | tail +2 | sort -n | tail -1", boot_disk
        )],
    ).await?.trim().parse().context("sfdisk End parse error")?;

    // Round up to a 1 MB (2048-sector) boundary.
    let first_bf_sector = ((last_part_end + 1 + 2047) / 2048) * 2048;
    let bf_num_sectors = first_mutable_sector - first_bf_sector;

    // Capture before repartitioning: sfdisk can change the disk identifier,
    // and fstab plus cmdline.txt still reference the old one.
    let orig_id = get_disk_identifier(boot_disk).await?;

    emitter.progress("Creating backingfiles partition...");
    sentryusb_shell::run(
        "bash", &["-c", &format!(
            "echo '{},{}' | sfdisk --force --no-reread {} -N {}",
            first_bf_sector, bf_num_sectors, boot_disk, last_part_num + 1
        )],
    ).await.context("sfdisk backingfiles failed")?;

    emitter.progress("Creating mutable partition...");
    sentryusb_shell::run(
        "bash", &["-c", &format!(
            "echo '{},' | sfdisk --force --no-reread {} -N {}",
            first_mutable_sector, boot_disk, last_part_num + 2
        )],
    ).await.context("sfdisk mutable failed")?;

    let _ = sentryusb_shell::run("partprobe", &[boot_disk]).await;
    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=30"]).await;

    if !Path::new(&bf_dev).exists() || !Path::new(&mut_dev).exists() {
        let _ = sentryusb_shell::run(
            "partx", &["--add", "--nr", &format!("{}:{}", last_part_num + 1, last_part_num + 2), boot_disk],
        ).await;
        let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=30"]).await;
    }

    if !Path::new(&bf_dev).exists() || !Path::new(&mut_dev).exists() {
        bail!("Failed to create partitions: {} or {} not found", bf_dev, mut_dev);
    }

    let new_id = get_disk_identifier(boot_disk).await?;
    if orig_id != new_id {
        emitter.progress("Updating disk identifier in fstab and cmdline.txt...");
        let fstab = std::fs::read_to_string("/etc/fstab").unwrap_or_default();
        std::fs::write("/etc/fstab", fstab.replace(&orig_id, &new_id))?;

        if let Some(cmdline) = &env.cmdline_path {
            if Path::new(cmdline).exists() {
                let content = std::fs::read_to_string(cmdline).unwrap_or_default();
                std::fs::write(cmdline, content.replace(&orig_id, &new_id))?;
            }
        }
    }

    // ~1 mutable inode per 20000 sectors of backingfiles.
    let mutable_inodes = bf_num_sectors / 20000;

    // Skip full-device TRIM and bound mkfs on slow or wedged media.
    let op_timeout = Duration::from_secs(120);
    emitter.progress(&format!("Formatting backingfiles (xfs) on {}...", bf_dev));
    sentryusb_shell::run_with_timeout(op_timeout, "mkfs.xfs",
        &["-f", "-K", "-m", "reflink=1", "-L", "backingfiles", &bf_dev]).await
        .context("mkfs.xfs failed")?;

    emitter.progress(&format!("Formatting mutable (ext4) on {}...", mut_dev));
    sentryusb_shell::run_with_timeout(op_timeout,
        "mkfs.ext4", &["-F", "-N", &mutable_inodes.to_string(), "-L", "mutable", &mut_dev],
    ).await.context("mkfs.ext4 failed")?;

    emitter.progress("Partition formatting complete.");
    update_fstab().await?;
    Ok(true)
}

async fn get_disk_identifier(disk: &str) -> Result<String> {
    let output = sentryusb_shell::run(
        "bash", &["-c", &format!(
            "fdisk -l {} | grep 'Disk identifier' | sed 's/Disk identifier: 0x//'", disk
        )],
    ).await?;
    Ok(output.trim().to_string())
}

/// A managed label resolved outside the configured DATA_DRIVE.
struct StaleLabel {
    label: &'static str,
    device: String,
    parent: String,
}

/// Return the first managed label that resolves outside `data_drive`.
async fn label_on_other_drive(data_drive: &str) -> Option<StaleLabel> {
    for label in &["backingfiles", "mutable"] {
        let symlink = format!("/dev/disk/by-label/{}", label);
        let Ok(target) = std::fs::read_link(&symlink) else { continue };
        // Resolve a relative target like "../../sda2" to "/dev/sda2".
        let resolved = std::path::Path::new("/dev/disk/by-label")
            .join(target)
            .canonicalize()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_default();
        if resolved.is_empty() {
            continue;
        }
        // /dev/sda2 -> /dev/sda, /dev/mmcblk0p3 -> /dev/mmcblk0
        let parent = strip_partition_suffix(&resolved);
        if !parent.is_empty() && parent != data_drive {
            return Some(StaleLabel {
                label,
                device: resolved,
                parent,
            });
        }
    }
    None
}

/// Return a partition's parent device, handling direct numeric and `pN`
/// suffixes without stripping digits from mmcblk/nvme parent names.
fn strip_partition_suffix(part: &str) -> String {
    let p_style = part.contains("mmcblk") || part.contains("nvme") || part.contains("loop");
    if p_style {
        // Strip only a terminal p<digits> partition suffix.
        if let Some(p_idx) = part.rfind('p') {
            let suffix = &part[p_idx + 1..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return part[..p_idx].to_string();
            }
        }
        return part.to_string();
    }
    // sd-style partitions append digits directly.
    part.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
}

async fn check_label_matches(device: &str, label: &str) -> bool {
    let symlink = format!("/dev/disk/by-label/{}", label);
    if let Ok(target) = std::fs::read_link(&symlink) {
        let target_str = target.to_string_lossy();
        let dev_name = Path::new(device).file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        target_str.ends_with(&dev_name)
    } else {
        false
    }
}

async fn check_fstype(device: &str, expected: &str) -> bool {
    sentryusb_shell::run("bash", &["-c", &format!(
        "blkid {} | grep -q 'TYPE=\"{}\"'", device, expected
    )]).await.is_ok()
}

async fn cleanup_mounts() {
    for mount in &["/mnt/cam", "/backingfiles", "/mutable"] {
        let _ = sentryusb_shell::run("umount", &[mount]).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// Release gadget, swap, mounts, loops, signatures, and kernel mappings before
/// rewriting a partition table.
async fn release_data_drive(drive: &str, emitter: &SetupEmitter) {
    // Drop gadget references to cam_disk.bin.
    let _ = sentryusb_gadget::disable();

    // -P protects mountpoints containing spaces.
    let lsblk_out = sentryusb_shell::run(
        "lsblk", &["-Pno", "NAME,MOUNTPOINT,FSTYPE", "-p", drive],
    ).await.unwrap_or_default();

    let mut parts: Vec<(String, String, String)> = Vec::new();
    for line in lsblk_out.lines() {
        let mut name = String::new();
        let mut mp = String::new();
        let mut fst = String::new();
        for field in line.split_whitespace() {
            if let Some(v) = field.strip_prefix("NAME=") {
                name = v.trim_matches('"').to_string();
            } else if let Some(v) = field.strip_prefix("MOUNTPOINT=") {
                mp = v.trim_matches('"').to_string();
            } else if let Some(v) = field.strip_prefix("FSTYPE=") {
                fst = v.trim_matches('"').to_string();
            }
        }
        if !name.is_empty() && name != drive {
            parts.push((name, mp, fst));
        }
    }

    // Disable swap partitions on the drive.
    for (name, _mp, fst) in &parts {
        if fst == "swap" {
            emitter.progress(&format!("swapoff {}", name));
            let _ = sentryusb_shell::run("swapoff", &[name]).await;
        }
    }

    // Detach managed and desktop-automounted paths even when a process holds one.
    for (name, mp, _fst) in &parts {
        if !mp.is_empty() && mp != "[SWAP]" {
            emitter.progress(&format!("Unmounting {} from {}", name, mp));
            let _ = sentryusb_shell::run("umount", &["-lf", mp]).await;
        }
    }

    // Detach loops backed by this drive.
    for (name, _mp, _fst) in &parts {
        let loops = sentryusb_shell::run("losetup", &["-j", name]).await.unwrap_or_default();
        for line in loops.lines() {
            if let Some(loop_dev) = line.split(':').next() {
                let _ = sentryusb_shell::run("losetup", &["-d", loop_dev]).await;
            }
        }
    }

    // Remove signatures so auto-probers do not reclaim partitions.
    for (name, _mp, _fst) in &parts {
        let _ = sentryusb_shell::run_with_timeout(
            Duration::from_secs(60), "wipefs", &["-afq", name],
        ).await;
    }

    // Drop kernel partition mappings.
    let _ = sentryusb_shell::run("partx", &["-d", drive]).await;

    // Drain pending udev events.
    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=10"]).await;

    // Flush and force a final partition-table reread.
    let _ = sentryusb_shell::run("blockdev", &["--flushbufs", drive]).await;
    let _ = sentryusb_shell::run("blockdev", &["--rereadpt", drive]).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// Append LABEL= entries for backingfiles and mutable if absent, leaving any
/// existing lines untouched, and create the mount points.
async fn update_fstab() -> Result<()> {
    let fstab = std::fs::read_to_string("/etc/fstab").unwrap_or_default();

    let mut additions = String::new();

    if !fstab.contains("LABEL=backingfiles") {
        additions.push_str(&format!(
            "LABEL=backingfiles {} xfs auto,rw,noatime,nofail 0 2\n", BACKINGFILES_MOUNT
        ));
    }
    if !fstab.contains("LABEL=mutable") {
        additions.push_str(&format!(
            "LABEL=mutable {} ext4 auto,rw,nofail 0 2\n", MUTABLE_MOUNT
        ));
    }

    if !additions.is_empty() {
        let mut new_fstab = fstab;
        if !new_fstab.ends_with('\n') {
            new_fstab.push('\n');
        }
        new_fstab.push_str(&additions);
        std::fs::write("/etc/fstab", new_fstab)?;
        info!("Updated /etc/fstab with backingfiles and mutable entries");
    }

    let _ = std::fs::create_dir_all(BACKINGFILES_MOUNT);
    let _ = std::fs::create_dir_all(MUTABLE_MOUNT);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_partition_suffix_handles_sd_style() {
        // Trailing digits attach directly to the device name.
        assert_eq!(strip_partition_suffix("/dev/sda1"), "/dev/sda");
        assert_eq!(strip_partition_suffix("/dev/sda12"), "/dev/sda");
        assert_eq!(strip_partition_suffix("/dev/sdb"), "/dev/sdb");
    }

    #[test]
    fn strip_partition_suffix_handles_p_style() {
        // A `p` separator precedes the digits.
        assert_eq!(strip_partition_suffix("/dev/mmcblk0p1"), "/dev/mmcblk0");
        assert_eq!(strip_partition_suffix("/dev/mmcblk0p11"), "/dev/mmcblk0");
        assert_eq!(strip_partition_suffix("/dev/nvme0n1p2"), "/dev/nvme0n1");
        assert_eq!(strip_partition_suffix("/dev/loop0p1"), "/dev/loop0");
    }

    #[test]
    fn strip_partition_suffix_no_digits_returns_input() {
        // Already a parent disk, so unchanged.
        assert_eq!(strip_partition_suffix("/dev/sda"), "/dev/sda");
        assert_eq!(strip_partition_suffix("/dev/mmcblk0"), "/dev/mmcblk0");
    }

    #[test]
    fn partition_prefix_routes_devices_correctly() {
        assert_eq!(partition_prefix("/dev/sda"), "");
        assert_eq!(partition_prefix("/dev/sdb"), "");
        assert_eq!(partition_prefix("/dev/mmcblk0"), "p");
        assert_eq!(partition_prefix("/dev/nvme0n1"), "p");
        assert_eq!(partition_prefix("/dev/loop0"), "p");
    }
}
