//! Partition management: detect, create, and format the backingfiles (XFS) and
//! mutable (ext4) partitions, and keep /etc/fstab in sync.

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

    // Detect a DATA_DRIVE swap with the old drive still attached: if
    // LABEL=backingfiles or LABEL=mutable resolves to a partition that does
    // NOT live on the new data_drive, the user changed DATA_DRIVE without
    // disconnecting the old disk. Proceeding would either wipe the old drive
    // (DATA LOSS) or leave a label conflict that makes `mount LABEL=...`
    // ambiguous, so refuse and let the user disconnect the old drive first.
    // The old data is left untouched.
    if let Some(stale) = label_on_other_drive(data_drive).await {
        bail!(
            "DATA_DRIVE is set to {} but the {} from a previous setup is still \
             attached at {}. Disconnect the old drive before re-running setup, \
             or change DATA_DRIVE back to {}. Your old drive will not be modified.",
            data_drive, stale.label, stale.device, stale.parent
        );
    }

    // Second guard against entering the destructive wipefs/parted/mkfs branch
    // on a system where setup previously completed. The runner's
    // skip_partitioning guard is the primary defense, but it depends on
    // partitions_exist(), whose label-symlink probe can momentarily miss on a
    // udev race. A FINISHED marker means the user already had a working
    // install, so a hard error always beats silently destroying their data.
    // Deleting the marker by hand is the deliberate opt-in to a wipe.
    let setup_finished = std::path::Path::new("/dashusb/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/firmware/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/DASHUSB_SETUP_FINISHED").exists();

    let bf_ok = check_label_matches(&p2, "backingfiles").await;
    let mut_ok = check_label_matches(&p1, "mutable").await;
    let bf_xfs = check_fstype(&p2, "xfs").await;
    let mut_ext4 = check_fstype(&p1, "ext4").await;

    let already_partitioned = bf_ok && mut_ok && bf_xfs && mut_ext4;

    // Idempotency: when the partitions already have the right labels and
    // filesystems, KEEP them and only (re)write fstab. Fstab is output, not
    // input: a missing LABEL= line is a 4 KB text repair, never a reason to
    // wipefs a TB of dashcam footage. A wizard re-run for a config-only change
    // (e.g. ARCHIVE_SERVER) lands here and never loses data.
    if already_partitioned {
        emitter.progress(&format!(
            "Existing backingfiles (xfs) and mutable (ext4) partitions found on {}. Keeping them.",
            data_drive
        ));
        // Quiesce anything holding the partitions open, then return. This path
        // must NOT run xfs_repair or mkfs: a repair that times out and falls
        // back to mkfs wipes the user, and even a safe repair blocks the
        // wizard for minutes on TB-class drives on every config-only re-run.
        // Mount replays the XFS log when needed; a genuinely broken log
        // surfaces as a clear mount error the user can recover from.
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

    // Everything below is destructive (wipefs, parted mktable, mkfs), which is
    // never right on an already-finished install: refuse loudly and leave the
    // data in place. Recovery path is to find why the labels or fstypes
    // drifted (often an unmounted partition or a transient blkid blip) and
    // re-run.
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

    // Covers the auto-mounters and loop devices that cleanup_mounts
    // (well-known paths only) misses. Without it, parted writes the new GPT
    // but the kernel refuses to switch to it because something still holds a
    // partition open, commonly udisks2 having auto-mounted the prior
    // install's partition at /media/pi/<label>.
    emitter.progress(&format!("Releasing kernel-side holders on {}...", data_drive));
    release_data_drive(data_drive, emitter).await;

    emitter.progress(&format!("WARNING: This will delete EVERYTHING on {}", data_drive));
    // Bound every block-device operation: a wedged USB bridge can hang wipefs
    // or parted indefinitely, leaving the wizard stuck on "Creating
    // partitions..." with no way to recover. 2 minutes clears any healthy
    // drive (mkfs.ext4 lazy-init finishes multi-TB drives in seconds).
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
    // -K skips the default full-device TRIM: slow on large media, useless on
    // a fresh partition.
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

    // Idempotency: if the partitions exist with the right labels, keep them
    // and only (re)write fstab. Fstab is output, not input.
    if partitions_exist().await {
        update_fstab().await?;
        return Ok(false);
    }

    // If setup previously finished, fresh partitions must never be carved on
    // the SD card. Bail rather than run sfdisk against a working install.
    // Same reasoning as the data-drive path above.
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

    // -K skips mkfs.xfs's default full-device TRIM. On a large, slow SD card
    // (1 TB on a Pi 3) discarding the backingfiles partition takes minutes and
    // trips the 30 s default command timeout, and the discard is useless on a
    // fresh partition. The explicit timeout stops a wedged card hanging the
    // wizard.
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

/// A DashUSB label that resolved to a partition on some disk other than the
/// configured DATA_DRIVE, carrying enough detail for the wizard to name the
/// old disk in its error message.
struct StaleLabel {
    label: &'static str,
    device: String,
    parent: String,
}

/// Returns the first of `backingfiles` or `mutable` whose label points at a
/// partition NOT belonging to `data_drive`. `None` means no conflict: either
/// no symlink, or it resolves onto the new data_drive.
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

/// Drop the trailing partition number from a partition device path.
/// `sd*` partitions suffix the number directly (sda2); `mmcblk*`, `nvme*`, and
/// `loop*` use a `p` separator (mmcblk0p3, nvme0n1p2).
///
/// Parent disks in the p-style families already end in a digit
/// (`/dev/mmcblk0`, `/dev/nvme0n1`), so trailing digits must NOT be stripped
/// universally: that would chop the `0` off `mmcblk0` and yield a
/// non-existent device. Dispatch on device family and strip the `p<digits>`
/// suffix only when it is present.
fn strip_partition_suffix(part: &str) -> String {
    let p_style = part.contains("mmcblk") || part.contains("nvme") || part.contains("loop");
    if p_style {
        // Strip exactly `p<digits>$`. No `p`, or non-digits after the last
        // `p`, means the input is already the parent disk.
        if let Some(p_idx) = part.rfind('p') {
            let suffix = &part[p_idx + 1..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return part[..p_idx].to_string();
            }
        }
        return part.to_string();
    }
    // sd-style: the parent ends in a letter and the partition number is a
    // trailing digit run with no separator.
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

/// Release every kernel-side reference to `drive` and its partitions before
/// the partition table is rewritten. The steps below run in order and mirror
/// what desktop "Disks" apps do before reformatting.
///
/// Required because `parted ... mktable` writes the new GPT to disk and then
/// asks the kernel to re-read it, and that ioctl fails ("...unable to inform
/// the kernel of the change, probably because it/they are in use") if anything
/// still holds a reference. Seen on a fresh boot where systemd/udisks2 had
/// auto-mounted the previous install's `mutable` partition at
/// `/media/pi/mutable`, which the well-known-paths cleanup never touches.
async fn release_data_drive(drive: &str, emitter: &SetupEmitter) {
    // Step 1: drop the USB gadget so configfs isn't holding cam_disk.bin
    // across the teardown.
    let _ = sentryusb_gadget::disable();

    // Snapshot every partition of this drive with its mountpoint and fstype.
    // -P quotes the pairs so spaces in mountpoints don't break parsing. The
    // parent device row is skipped below.
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

    // Step 2: swapoff any swap partition on the drive.
    for (name, _mp, fst) in &parts {
        if fst == "swap" {
            emitter.progress(&format!("swapoff {}", name));
            let _ = sentryusb_shell::run("swapoff", &[name]).await;
        }
    }

    // Step 3: lazy-force-unmount every active mountpoint anywhere on the
    // system that lives on a partition of this drive (/media/pi/<label>,
    // /run/media/<user>/<label>, custom locations). Lazy plus force covers a
    // process still holding the directory open: the mount detaches from the
    // namespace immediately so parted can proceed, and the fd is reaped when
    // the process exits.
    for (name, mp, _fst) in &parts {
        if !mp.is_empty() && mp != "[SWAP]" {
            emitter.progress(&format!("Unmounting {} from {}", name, mp));
            let _ = sentryusb_shell::run("umount", &["-lf", mp]).await;
        }
    }

    // Step 4: detach loop devices backed by partitions of this drive. -j
    // prints the matching loop devices, which are then detached with -d.
    for (name, _mp, _fst) in &parts {
        let loops = sentryusb_shell::run("losetup", &["-j", name]).await.unwrap_or_default();
        for line in loops.lines() {
            if let Some(loop_dev) = line.split(':').next() {
                let _ = sentryusb_shell::run("losetup", &["-d", loop_dev]).await;
            }
        }
    }

    // Step 5: wipe FS signatures on each partition. Stops auto-probers
    // (udisks2, blkid, autofs) re-grabbing the partition between the umount
    // above and parted's BLKRRPART.
    for (name, _mp, _fst) in &parts {
        let _ = sentryusb_shell::run_with_timeout(
            Duration::from_secs(60), "wipefs", &["-afq", name],
        ).await;
    }

    // Step 6: drop kernel partition table mappings.
    let _ = sentryusb_shell::run("partx", &["-d", drive]).await;

    // Step 7: let pending udev change events finish before touching the disk.
    let _ = sentryusb_shell::run("udevadm", &["settle", "--timeout=10"]).await;

    // Step 8: flush the page cache and force a partition-table reread. If
    // rereadpt still fails here, parted will too, with a clearer error.
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
