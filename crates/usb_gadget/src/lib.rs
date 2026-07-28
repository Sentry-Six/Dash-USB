//! USB gadget control via Linux configfs.
//!
//! Replaces `enable_gadget.sh` and `disable_gadget.sh` with native Rust
//! operations on `/sys/kernel/config/usb_gadget/dashusb`.

pub mod cycle_lock;
pub mod snapshot;
pub mod space;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tracing::info;

const GADGET_NAME: &str = "dashusb";
// US English. MUST stay spelled `0x409`, matching the shell path. The kernel
// parses `0x0409` and `0x409` to the same langid, but the configfs dentry
// keeps whichever string was mkdir'd first. Spelling it `0x0409` here means
// `disable()` cannot rmdir `strings/0x409`, the orphan dir pins libcomposite
// forever, and the next `enable()` gets EEXIST.
const LANG: &str = "0x409";
const CFG: &str = "c";

/// (backing image path, inquiry label) exposed as USB mass storage LUNs.
const DISK_IMAGES: &[(&str, &str)] = &[
    ("/backingfiles/cam_disk.bin", "CAM"),
];

fn find_configfs_root() -> Result<PathBuf> {
    let mounts = fs::read_to_string("/proc/mounts")
        .context("failed to read /proc/mounts")?;
    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 3 && fields[2] == "configfs" {
            return Ok(PathBuf::from(fields[1]));
        }
    }
    bail!("configfs not mounted")
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Create `link -> target`, replacing any stale entry at `link` first.
///
/// Detect that entry with `symlink_metadata()`, which does NOT follow
/// symlinks. A dangling link (a previous `disable()` left
/// `configs/c.1/mass_storage.0` pointing at a torn-down
/// `functions/mass_storage.0`) makes `Path::exists()` report false because it
/// follows the link to the missing target; `symlink()` then fails with EEXIST
/// because the link path itself still exists.
#[cfg(unix)]
fn ensure_symlink(target: &Path, link: &Path) -> Result<()> {
    match link.symlink_metadata() {
        Ok(_) => fs::remove_file(link)
            .with_context(|| format!("failed to remove stale symlink {}", link.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("failed to stat {}", link.display())),
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("failed to symlink {} -> {}", link.display(), target.display()))
}

#[cfg(not(unix))]
fn ensure_symlink(_target: &Path, _link: &Path) -> Result<()> {
    bail!("USB gadget control requires Linux")
}

/// MaxPower in mA for the SBC model reported by the device tree.
fn get_max_power() -> u32 {
    let model = fs::read_to_string("/proc/device-tree/model").unwrap_or_default();
    let model = model.to_lowercase();
    if model.contains("pi 5") {
        600
    } else if model.contains("pi 4") {
        500
    } else if model.contains("pi 3") {
        300
    } else if model.contains("pi 2") || model.contains("zero 2") {
        200
    } else {
        100
    }
}

/// Serial `DashUSB-<hex sha256(machine-id)>`: a stable per-device identity
/// that some head units cache across replugs.
fn get_machine_serial() -> String {
    let mid = fs::read_to_string("/etc/machine-id").unwrap_or_default();
    let mid = mid.trim();
    if mid.is_empty() {
        return "DashUSB-unknown".to_string();
    }
    let h = ring::digest::digest(&ring::digest::SHA256, mid.as_bytes());
    format!("DashUSB-{}", hex::encode(h.as_ref()))
}

/// True if a gadget dir is complete enough to re-bind: the mass_storage
/// function exists and its `lun.0/file` is readable and non-empty. Anything
/// weaker means a prior enable crashed mid-setup; rebuild from scratch.
fn gadget_dir_is_complete(gadget: &Path) -> bool {
    let func = gadget.join("functions/mass_storage.0");
    let lun0_file = func.join("lun.0/file");
    match fs::read_to_string(&lun0_file) {
        Ok(s) => !s.trim().is_empty(),
        Err(_) => false,
    }
}

/// Configure and bind the gadget. Equivalent to `enable_gadget.sh`.
pub fn enable() -> Result<()> {
    let configfs = find_configfs_root()?;
    let gadget = configfs.join("usb_gadget").join(GADGET_NAME);

    // Unload the legacy single-function g_mass_storage gadget before
    // assembling the composite one; while loaded it holds the UDC.
    let _ = std::process::Command::new("modprobe")
        .args(["-q", "-r", "g_mass_storage"])
        .status();

    // Existing and complete: only a UDC (re)bind is needed, since a prior
    // enable can fail to bind on a busy UDC yet leave a valid config.
    // Existing but INCOMPLETE (crashed mid-enable): tear down and rebuild,
    // because binding a half-configured gadget enumerates a device with no
    // LUNs.
    if gadget.exists() {
        if gadget_dir_is_complete(&gadget) {
            // Kernel 6.18+ closes the LUN backing file on UDC unbind, so a
            // rebind without refreshing the LUN reports "(no medium)". Clear
            // and rewrite each LUN's own current path so the kernel re-opens
            // it. LUN numbering is compact (enable() skips missing images),
            // so indexing DISK_IMAGES positionally would refresh the wrong
            // LUN, or none, whenever an image is absent.
            let func_dir = gadget.join("functions/mass_storage.0");
            for i in 0..DISK_IMAGES.len() {
                let lun_file = func_dir.join(format!("lun.{}/file", i));
                let current = match fs::read_to_string(&lun_file) {
                    Ok(s) => s.trim().to_string(),
                    Err(_) => continue,
                };
                if current.is_empty() || !Path::new(&current).exists() {
                    continue;
                }
                let _ = fs::write(&lun_file, "\n");
                // Re-apply the profile's nofua setting while the medium is
                // detached: a gadget dir built by an older version predates
                // this policy (the fresh-build path below explains why the
                // car needs it).
                let nofua = if sentryusb_vehicle_profile::Profile::active().features.nofua {
                    "1"
                } else {
                    "0"
                };
                let _ = fs::write(func_dir.join(format!("lun.{}/nofua", i)), nofua);
                std::thread::sleep(std::time::Duration::from_secs(1));
                let _ = fs::write(&lun_file, &current);
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
            return bind_udc(&gadget);
        }
        info!("USB gadget dir exists but is incomplete — tearing down and rebuilding");
        disable()?;
    }

    // One modprobe call per module. On kernel 6.18+, passing both on a single
    // command line parses the second name as a module parameter instead of
    // loading it: `libcomposite: unknown parameter 'usb_f_mass_storage'
    // ignored`.
    let _ = std::process::Command::new("modprobe")
        .arg("libcomposite")
        .status();
    let _ = std::process::Command::new("modprobe")
        .arg("usb_f_mass_storage")
        .status();

    let cfg_dir = gadget.join(format!("configs/{}.1", CFG));
    fs::create_dir_all(&cfg_dir)
        .with_context(|| format!("failed to create {}", cfg_dir.display()))?;

    write_file(&gadget.join("idVendor"), "0x1d6b")?;  // Linux Foundation
    write_file(&gadget.join("idProduct"), "0x0104")?;  // Composite Gadget
    write_file(&gadget.join("bcdDevice"), "0x0100")?;  // v1.0.0
    write_file(&gadget.join("bcdUSB"), "0x0200")?;     // USB 2.0

    let strings_dir = gadget.join(format!("strings/{}", LANG));
    fs::create_dir_all(&strings_dir)
        .with_context(|| format!("failed to create {}", strings_dir.display()))?;
    let cfg_strings = gadget.join(format!("configs/{}.1/strings/{}", CFG, LANG));
    fs::create_dir_all(&cfg_strings)
        .with_context(|| format!("failed to create {}", cfg_strings.display()))?;

    write_file(&strings_dir.join("serialnumber"), &get_machine_serial())?;
    write_file(&strings_dir.join("manufacturer"), "DashUSB")?;
    write_file(&strings_dir.join("product"), "DashUSB Composite Gadget")?;
    write_file(&cfg_strings.join("configuration"), "DashUSB Config")?;

    write_file(
        &cfg_dir.join("MaxPower"),
        &get_max_power().to_string(),
    )?;

    let func_dir = gadget.join("functions/mass_storage.0");
    fs::create_dir_all(&func_dir)
        .with_context(|| format!("failed to create {}", func_dir.display()))?;

    let mut lun = 0;
    for (image_path, label) in DISK_IMAGES {
        if Path::new(image_path).exists() {
            let lun_dir = func_dir.join(format!("lun.{}", lun));
            // Create every LUN dir including lun.0: depending on the kernel's
            // configfs version, lun.0 is NOT guaranteed to be auto-created
            // when the mass_storage function is instantiated, and writing to
            // `lun.0/file` before the dir exists silently fails.
            fs::create_dir_all(&lun_dir)
                .with_context(|| format!("failed to create lun.{} at {}", lun, lun_dir.display()))?;
            // The car issues FUA (force-unit-access) writes, which the
            // mass_storage function honors as synchronous flushes through its
            // single-threaded worker. One slow flush under disk contention can
            // exceed the car's SCSI timeout, making it drop the drive (X on
            // the cam icon) until it is re-plugged. nofua=1 lets FUA writes
            // complete as normal cached writes; the images are fsck'd on
            // every gadget cycle, so that integrity tradeoff is already priced in.
            //
            // Best-effort: a kernel whose mass_storage function lacks the
            // nofua attribute must not fail the whole enable. Missing nofua
            // only leaves FUA stalls possible; failing here would leave the
            // car with no drive at all.
            let nofua = if sentryusb_vehicle_profile::Profile::active().features.nofua {
                "1"
            } else {
                "0"
            };
            if let Err(e) = write_file(&lun_dir.join("nofua"), nofua) {
                tracing::warn!("could not set nofua on lun.{lun}: {e:#}");
            }
            write_file(&lun_dir.join("file"), image_path)?;

            let size = fs::metadata(image_path)
                .map(|m| format_size(m.len()))
                .unwrap_or_else(|_| "?".to_string());
            write_file(
                &lun_dir.join("inquiry_string"),
                &format!("DashUSB {} {}", label, size),
            )?;

            lun += 1;
        }
    }

    // Link the function into the config; ensure_symlink also clears a
    // dangling link left behind by a previous teardown.
    ensure_symlink(&func_dir, &cfg_dir.join("mass_storage.0"))?;

    info!("USB gadget configured with {} LUN(s)", lun);

    // Kernel 6.18+ needs the configfs LUN file-attribute writes to propagate
    // before the UDC bind activates the mass_storage function. Without the
    // delay the function activates with "LUN: removable file: (no medium)"
    // even though the file attribute reads back correctly. 3 s is the
    // measured minimum on rockchip64.
    std::thread::sleep(std::time::Duration::from_secs(3));

    bind_udc(&gadget)
}

/// Bind (or rebind) the UDC for an already-configured gadget dir. On a busy
/// UDC, blank the slot, wait briefly and retry so stale bindings clear;
/// returns the underlying error if the final attempt fails.
fn bind_udc(gadget: &Path) -> Result<()> {
    let udc = find_udc()?;
    let udc_path = gadget.join("UDC");

    // Clear any stale binding before writing the new one.
    let _ = fs::write(&udc_path, "");

    for attempt in 1..=5 {
        match fs::write(&udc_path, &udc) {
            Ok(()) => {
                // A sysfs write to `UDC` can return Ok even when the kernel
                // silently rejected the bind (incomplete gadget config, UDC
                // refused attachment). Read back to confirm the binding stuck
                // and treat a mismatch as retryable, not a silent success.
                match fs::read_to_string(&udc_path) {
                    Ok(s) if s.trim() == udc.trim() => {
                        info!("USB gadget bound to UDC: {}", udc);
                        return Ok(());
                    }
                    Ok(other) if attempt < 5 => {
                        info!(
                            "UDC bind attempt {} wrote {:?} but sysfs reads back {:?}; retrying",
                            attempt, udc, other.trim()
                        );
                        let _ = fs::write(&udc_path, "");
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Ok(other) => {
                        return Err(anyhow::anyhow!(
                            "UDC bind silently rejected: wrote {:?}, readback {:?}",
                            udc,
                            other.trim()
                        ));
                    }
                    Err(_) => {
                        // UDC unreadable after a successful write: trust the
                        // write rather than false-failing.
                        info!("USB gadget bound to UDC: {} (readback failed)", udc);
                        return Ok(());
                    }
                }
            }
            Err(e) if attempt < 5 => {
                info!("UDC bind attempt {} failed ({}), retrying", attempt, e);
                let _ = fs::write(&udc_path, "");
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to bind UDC {}: {}", udc, e));
            }
        }
    }
    Ok(())
}

/// Tear the gadget down in configfs. Equivalent to `disable_gadget.sh`.
pub fn disable() -> Result<()> {
    // Unload g_mass_storage FIRST so it releases the UDC before the UDC is
    // deactivated. Left to the end, the kernel can keep the UDC bound, the
    // UDC write below silently no-ops, and the next `enable()` hangs on
    // "UDC busy" forever.
    let _ = std::process::Command::new("modprobe")
        .args(["-q", "-r", "g_mass_storage"])
        .status();

    let configfs = find_configfs_root()?;
    let gadget = configfs.join("usb_gadget").join(GADGET_NAME);

    if !gadget.exists() {
        info!("USB gadget already disabled");
        return Ok(());
    }

    // Deactivate the UDC by writing a newline, NOT a zero-byte string: some
    // configfs UDC handlers reject empty writes outright. An already-unbound
    // gadget (a prior `disable()` that ran halfway, or boot before the first
    // enable) returns ENODEV, which is harmless and discarded.
    let _ = fs::write(gadget.join("UDC"), "\n");

    // Detach the function from the configuration first. While this symlink
    // exists the kernel treats the function as in use: LUN `file` attributes
    // are pinned read-only with EBUSY and `rmdir functions/mass_storage.0`
    // fails. Removing the symlink unblocks the rest of the cascade.
    let cfg_dir = gadget.join(format!("configs/{}.1", CFG));
    let _ = fs::remove_file(cfg_dir.join("mass_storage.0"));
    let cfg_strings = cfg_dir.join(format!("strings/{}", LANG));
    let _ = fs::remove_dir(&cfg_strings);
    // Compat: binaries predating the LANG fix used `0x0409`, so installs that
    // routed archiveloop through Rust (install-pi.sh shim, `dashusb gadget
    // enable` CLI shim) have a literal `strings/0x0409` dir. Remove it too, or
    // a hot-upgraded install keeps the orphan and pins libcomposite forever.
    // NotFound is expected on shell-script installs.
    let _ = fs::remove_dir(cfg_dir.join("strings/0x0409"));

    // With the function detached, clear each LUN's `file` attribute to
    // release its backing-file handle. Kernels that cascade-clean the
    // function once its last symlink is removed (Pi 5 / Linux 6.x) may have
    // dropped the LUN paths already; those writes are silently ignored. On
    // kernels that do not cascade, this is what lets the LUN/function rmdirs
    // below succeed instead of hitting EBUSY.
    let func_dir = gadget.join("functions/mass_storage.0");
    for i in 0..=4 {
        let _ = fs::write(func_dir.join(format!("lun.{}/file", i)), "\n");
    }

    // Remove the non-default LUNs (lun.1 through lun.4) only. lun.0 is the
    // *implicit* default LUN that the mass_storage function creates as part of
    // its own configfs node: on most kernels `rmdir lun.0` returns EPERM, and
    // lun.0 is released only when the parent `mass_storage.0` is removed.
    // Attempting it anyway leaves lun.0 in place, so `rmdir mass_storage.0`
    // fails, the gadget-root rmdir fails, configfs keeps pinning
    // `libcomposite`, and the next `enable()` bails out on "Module
    // libcomposite is in use" from `modprobe -r`. The web-UI toggle then looks
    // like it errored and only a reboot unsticks it.
    for i in 1..=4 {
        let _ = fs::remove_dir(func_dir.join(format!("lun.{}", i)));
    }
    let _ = fs::remove_dir(&func_dir);

    let _ = fs::remove_dir(&cfg_dir);
    let _ = fs::remove_dir(gadget.join(format!("strings/{}", LANG)));
    let _ = fs::remove_dir(gadget.join("strings/0x0409")); // legacy form, see above
    let _ = fs::remove_dir(&gadget);

    // Unload the composite and function modules (g_mass_storage went first).
    let _ = std::process::Command::new("modprobe")
        .args(["-r", "usb_f_mass_storage", "g_ether", "usb_f_ecm", "usb_f_rndis", "libcomposite"])
        .status();

    // Every rmdir above is best-effort (`let _ =`), so residue from
    // kernel-version quirks (lun.0 implicit-default behavior, cascade timing)
    // is invisible to the caller. Log it so flakes stay diagnosable.
    if gadget.exists() {
        tracing::warn!(
            "disable() completed but {} still present (incomplete teardown)",
            gadget.display()
        );
    }

    info!("USB gadget disabled");
    Ok(())
}

/// True only when the gadget is bound to a UDC AND `lun.0/file` is populated.
///
/// Both signals are required. A UDC-only check reports "active" for a gadget
/// that is bound but has lost its LUN backing file (a manual tear-down that
/// removed `lun.0/file` without unbinding the UDC), so the idempotent
/// `gadget_enable` API handler skips the full rebuild and the car stays
/// plugged into a device with no LUNs. A partially-torn-down gadget must
/// report inactive so the next enable call reconstructs it.
pub fn is_active() -> bool {
    let root = Path::new("/sys/kernel/config/usb_gadget/dashusb");
    let udc_bound = fs::read_to_string(root.join("UDC"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !udc_bound {
        return false;
    }
    gadget_dir_is_complete(root)
}

/// First available UDC (USB Device Controller) under /sys/class/udc.
fn find_udc() -> Result<String> {
    let udc_dir = Path::new("/sys/class/udc");
    if let Ok(entries) = fs::read_dir(udc_dir) {
        for entry in entries.flatten() {
            return Ok(entry.file_name().to_string_lossy().to_string());
        }
    }
    bail!("no UDC found in /sys/class/udc")
}

/// Format a byte count as human-readable (e.g., "32G", "512M").
fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{}G", bytes / 1_073_741_824)
    } else if bytes >= 1_048_576 {
        format!("{}M", bytes / 1_048_576)
    } else if bytes >= 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{}B", bytes)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn ensure_symlink_creates_fresh_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        ensure_symlink(&target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn ensure_symlink_replaces_valid_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        ensure_symlink(&target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn ensure_symlink_replaces_dangling_link() {
        // EEXIST regression: a teardown left the symlink pointing at a
        // function dir that no longer exists. `Path::exists()` follows the
        // link and returns false, so a plain `symlink()` then fails EEXIST.
        let dir = tempfile::tempdir().unwrap();
        let stale_target = dir.path().join("gone");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&stale_target, &link).unwrap();
        assert!(!link.exists(), "dangling link should report not-existent");
        let new_target = dir.path().join("real");
        fs::create_dir(&new_target).unwrap();
        ensure_symlink(&new_target, &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), new_target);
    }
}
