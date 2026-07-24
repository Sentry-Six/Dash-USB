//! Snapshot management — reflink-backed copy-on-write captures of the
//! cam disk image, plus the bookkeeping that makes those captures
//! browseable from the web UI, the phone app, and `/mutable/Recordings/`.
//!
//! Flow per snapshot: `cp --reflink` the live image, optional fsck,
//! read-only mount through autofs, TOC-diff against the previous
//! snapshot (identical ⇒ discarded), then walk the vehicle profile's
//! recording root and drop one symlink per matching clip under
//! `/mutable/Recordings/Continuous/<YYYY-MM-DD>/` — that link tree is
//! what archiveloop archives and the Viewer plays. The car firmware
//! rolling-deletes footage on the live drive (2 h on GM); snapshots
//! taken inside that window are how footage outlives it.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use tracing::{info, warn};

const SNAPSHOTS_DIR: &str = "/backingfiles/snapshots";
const CAM_DISK: &str = "/backingfiles/cam_disk.bin";
const REBUILD_FLAG: &str = "/mutable/.rebuild_snapshot_symlinks";

const RECORDINGS: &str = "/mutable/Recordings";

/// Create a snapshot of the cam disk plus the symlink/TOC bookkeeping
/// the archive loop and the web Viewer need.
///
/// `skip_fsck` corresponds to the `nofsck` arg the bash wrapper used to
/// pass after a reboot to avoid running fsck twice in quick succession.
///
/// Returns `Some(name)` on a fresh snapshot, `None` when the new snapshot
/// is byte-equivalent to the previous one (in which case we delete the
/// reflink to avoid accumulating identical copies).
pub async fn make_snapshot(skip_fsck: bool) -> Result<Option<String>> {
    let _ = std::fs::create_dir_all(SNAPSHOTS_DIR);

    if !Path::new(CAM_DISK).exists() {
        bail!("cam disk image not found at {}", CAM_DISK);
    }

    // ── pick the next snap-NNNNNN slot ────────────────────────────────
    // If the previous snapshot has no `.toc` it was abandoned mid-flight
    // — wipe it and reuse the slot.
    let (snap_num, prev_toc) = pick_next_snapshot_slot()?;
    let snap_name = format!("snap-{:06}", snap_num);
    let snap_dir = format!("{}/{}", SNAPSHOTS_DIR, snap_name);
    let snap_file = format!("{}/snap.bin", snap_dir);
    let snap_mnt = format!("/tmp/snapshots/{}", snap_name);
    let snap_mnt_link = format!("{}/mnt", snap_dir);

    std::fs::create_dir_all(&snap_dir)?;
    info!("Taking snapshot of cam disk in {}", snap_dir);

    // ── reflink copy (bash line 313) ──────────────────────────────────
    // `--reflink=auto` so non-XFS backingfiles (rare — setup wizard XFS
    // verify usually catches this) still works at the cost of a full copy.
    // Low I/O priority: the copy runs while the car may be writing dashcam
    // footage to the same disk through the gadget; at default priority it
    // can stall those writes past the car's SCSI timeout. Best-effort lowest
    // (-c2 -n7) rather than idle (-c3) so the copy still makes progress under
    // continuous sentry writes (needs the bfq scheduler to have effect).
    let cp_result = sentryusb_shell::run_with_timeout(
        Duration::from_secs(600),
        "ionice",
        &["-c2", "-n7", "nice", "-n19", "cp", "--reflink=auto", CAM_DISK, &snap_file],
    )
    .await;
    if let Err(e) = cp_result {
        let _ = std::fs::remove_dir_all(&snap_dir);
        bail!("cp --reflink failed: {}", e);
    }

    // ── optional fsck on the loop-mounted partition (bash 281-289) ────
    if !skip_fsck {
        if let Err(e) = fsck_snapshot(&snap_file).await {
            warn!("fsck on {} failed (non-fatal): {}", snap_file, e);
        }
    }

    // ── 32-bit Bookworm timestamp fix (bash 292-299) ──────────────────
    if cfg!(target_pointer_width = "32") {
        let _ = apply_bookworm_32bit_timestamp_fix(&snap_file).await;
    }

    // ── wait for autofs (bash 301-305) ────────────────────────────────
    // Symlinks we're about to create resolve through /tmp/snapshots/...
    // which is the autofs mount root. autofs needs to be active before
    // we touch the path or `find` below would just see an empty dir.
    wait_for_autofs().await;

    info!("Took snapshot {}", snap_name);

    // ── generate TOC for the freshly mounted snapshot (bash 309) ──────
    // Touch the autofs path first so the disk image is mounted before
    // `find` traverses it.
    let _ = sentryusb_shell::run("ls", &[&format!("{}/", snap_mnt)]).await;

    let toc_path = format!("{}.toc", snap_file);
    let toc_path_tmp = format!("{}_", toc_path);
    if let Err(e) = generate_toc(&snap_mnt, &toc_path_tmp).await {
        warn!("toc generation failed for {}: {}", snap_mnt, e);
    }

    // ── diff against previous snapshot's TOC (bash 310-311) ───────────
    // If nothing new is in this snapshot vs. the prior one, this is a
    // duplicate — release it and return None so callers don't think
    // they got a fresh snapshot.
    let is_new = match prev_toc.as_ref() {
        Some(prev) => toc_has_additions(prev, &toc_path_tmp).unwrap_or(true),
        None => true,
    };

    if !is_new {
        info!("Snapshot {} identical to previous; discarding", snap_name);
        let _ = std::fs::remove_file(&toc_path_tmp);
        let _ = std::fs::remove_file(&snap_file);
        let _ = std::fs::remove_dir_all(&snap_dir);
        return Ok(None);
    }

    // The car firmware rolling-deletes footage on the live drive.
    // Deletions are never mirrored into the snapshot link trees —
    // preserving footage past the car's window is the whole product.

    // ── Pre-create the <snapdir>/mnt symlink (bash 317) ───────────────
    // make_links_for_snapshot links each clip with a target like
    // <snapdir>/mnt/TeslaCam/...  ; if the symlink doesn't exist yet
    // those per-clip symlinks resolve to nothing until autofs gets
    // poked, which is fragile. Create it explicitly.
    if !Path::new(&snap_mnt_link).exists() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&snap_mnt, &snap_mnt_link);
    }

    // ── build /mutable/Recordings/... symlinks ────────────────────────
    if let Err(e) = make_links_for_snapshot(&snap_mnt, &snap_mnt_link) {
        warn!("make_links_for_snapshot failed: {}", e);
    }

    // ── commit the TOC (bash 319) ─────────────────────────────────────
    let _ = std::fs::rename(&toc_path_tmp, &toc_path);

    // ── rebuild-all if the flag file is present (bash 336-339) ────────
    if Path::new(REBUILD_FLAG).exists() {
        if let Err(e) = rebuild_all_snapshot_links() {
            warn!("rebuild_all_snapshot_links: {}", e);
        }
        let _ = std::fs::remove_file(REBUILD_FLAG);
    }

    Ok(Some(snap_name))
}

/// Normalize a snapshot identifier to its bare `snap-NNNNNN` name.
///
/// Callers pass either a bare name (`snap-000001`, e.g. from autofs) or a
/// full path under the snapshots dir (`/backingfiles/snapshots/snap-000001`,
/// e.g. the WebUI delete handler and `make_snapshot.sh`'s discard path). We
/// take the final path component so every form works. Taking the basename
/// also neutralizes any `..` traversal in the input — only the last
/// component is ever used, then appended to `SNAPSHOTS_DIR`.
fn normalize_snap_name(input: &str) -> Option<String> {
    let name = Path::new(input).file_name()?.to_str()?;
    if name.starts_with("snap-") && !name.contains("..") {
        Some(name.to_string())
    } else {
        None
    }
}

/// Release (delete) a snapshot. Accepts a bare `snap-NNNNNN` name or a full
/// path under the snapshots dir (see [`normalize_snap_name`]).
pub async fn release_snapshot(snap_name: &str) -> Result<()> {
    let name = match normalize_snap_name(snap_name) {
        Some(n) => n,
        None => bail!("invalid snapshot name: {}", snap_name),
    };

    let snap_dir = format!("{}/{}", SNAPSHOTS_DIR, name);
    if !Path::new(&snap_dir).exists() {
        bail!("snapshot not found: {}", name);
    }

    let mnt_dir = format!("{}/mnt", snap_dir);
    if Path::new(&mnt_dir).exists() {
        let _ = sentryusb_shell::run("umount", &[&mnt_dir]).await;
    }

    std::fs::remove_dir_all(&snap_dir)?;
    info!("Released snapshot: {}", name);
    Ok(())
}

/// List all snapshots.
pub fn list_snapshots() -> Vec<String> {
    let mut snaps = Vec::new();
    if let Ok(entries) = std::fs::read_dir(SNAPSHOTS_DIR) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("snap-") && entry.path().is_dir() {
                snaps.push(name);
            }
        }
    }
    snaps.sort();
    snaps
}

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/// Find the next free `snap-NNNNNN` slot. If the previous snapshot
/// looks abandoned (no `.toc` file, snap.bin missing), reuse its
/// number — bash matches this behaviour around line 295-300.
///
/// Returns `(snap_num, Option<previous_toc_path>)`. The previous TOC
/// is `None` on a brand-new install (no completed snapshots yet).
fn pick_next_snapshot_slot() -> Result<(u32, Option<String>)> {
    let mut max_num: u32 = 0;
    if let Ok(entries) = std::fs::read_dir(SNAPSHOTS_DIR) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num_str) = name.strip_prefix("snap-") {
                if let Ok(num) = num_str.parse::<u32>() {
                    if num > max_num {
                        max_num = num;
                    }
                }
            }
        }
    }

    if max_num == 0 {
        return Ok((1, None));
    }

    let prev_dir = format!("{}/snap-{:06}", SNAPSHOTS_DIR, max_num);
    let prev_toc = format!("{}/snap.bin.toc", prev_dir);
    let prev_bin = format!("{}/snap.bin", prev_dir);

    // Abandoned: no TOC was committed → reuse this slot.
    if !Path::new(&prev_toc).exists() || !Path::new(&prev_bin).exists() {
        let _ = std::fs::remove_dir_all(&prev_dir);
        let next = max_num;
        // Look one further back for a usable previous TOC.
        let backstop = if next > 1 {
            let p = format!("{}/snap-{:06}/snap.bin.toc", SNAPSHOTS_DIR, next - 1);
            if Path::new(&p).exists() { Some(p) } else { None }
        } else {
            None
        };
        return Ok((next, backstop));
    }

    Ok((max_num + 1, Some(prev_toc)))
}

/// fsck the snapshot's filesystem partition via a temporary loop device.
/// Mirrors bash lines 281-289. Failures are logged but non-fatal —
/// `archive-clips` will still run; we'd rather lose strict verification
/// of one snapshot than abort the whole archive cycle.
async fn fsck_snapshot(snap_file: &str) -> Result<()> {
    let loop_dev = losetup_find_show(snap_file).await?;
    let part = format!("{}p1", loop_dev);

    // `-p` works for both vfat and exfat. Output goes to stderr; we
    // surface a non-zero exit but don't propagate it.
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(120),
        "fsck",
        &[&part, "--", "-p"],
    )
    .await;

    let _ = sentryusb_shell::run("losetup", &["-d", &loop_dev]).await;
    Ok(())
}

/// Wrapper around `losetup -f -P --show <file>` with a small retry
/// loop, mirroring `losetup_find_show` in
/// `Sentry-USB/setup/pi/envsetup.sh:232-254`. Some kernels race on
/// the partition probe and return a device that isn't ready yet.
async fn losetup_find_show(file: &str) -> Result<String> {
    for attempt in 0..5 {
        let out = sentryusb_shell::run("losetup", &["-f", "-P", "--show", file]).await;
        match out {
            Ok(s) => {
                let dev = s.trim().to_string();
                if !dev.is_empty() && Path::new(&dev).exists() {
                    return Ok(dev);
                }
            }
            Err(_) if attempt < 4 => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => bail!("losetup failed: {}", e),
        }
    }
    bail!("losetup did not produce a usable device for {}", file)
}

/// Wait for autofs to be active before we hand it work. Capped at
/// 30 retries (~30s) so a misconfigured system doesn't hang archive
/// indefinitely.
async fn wait_for_autofs() {
    for _ in 0..30 {
        if sentryusb_shell::run("systemctl", &["--quiet", "is-active", "autofs"])
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    warn!("autofs is not active after 30s; symlinks may dangle");
}

/// Run `find <root> -type f -printf '%s %P\n'` and write the result to
/// `out_path`. Format is `<size> <relative-path>` per line, matching the
/// bash TOC produced at line 309.
async fn generate_toc(root: &str, out_path: &str) -> Result<()> {
    let cmd = format!(
        "find {} -type f -printf '%s %P\\n' > {}",
        shell_escape(root),
        shell_escape(out_path)
    );
    sentryusb_shell::run("bash", &["-c", &cmd])
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("find/toc: {}", e))
}

fn shell_escape(s: &str) -> String {
    // Just single-quote: snap paths are well-known and don't contain quotes.
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Returns true if `new_toc` has any line that isn't in `old_toc`.
/// Mirrors the bash `diff old new | grep -qe '^>'` check at line 310.
/// Lines are `<size> <path>`, compared whole: a clip that merely grew
/// (same name, new size — e.g. it was still being written during the
/// previous snapshot) must count as new, otherwise the fuller copy gets
/// discarded as a duplicate.
fn toc_has_additions(old_toc: &str, new_toc: &str) -> Result<bool> {
    let old = std::fs::read_to_string(old_toc).unwrap_or_default();
    let new = std::fs::read_to_string(new_toc)?;
    let old_set: std::collections::HashSet<&str> =
        old.lines().filter(|l| !l.is_empty()).collect();
    Ok(new
        .lines()
        .any(|line| !line.is_empty() && !old_set.contains(line)))
}

/// Build `/mutable/Recordings/Continuous/<YYYY-MM-DD>/` symlinks
/// pointing into the snapshot mount, per the active vehicle profile.
///
/// `cur_mnt` is `/tmp/snapshots/snap-NNNNNN` (autofs path used during
/// the scan). `final_mnt` is `<snapdir>/mnt` — the symlink to the autofs
/// path. Per-clip symlinks are retargeted onto `final_mnt` so they keep
/// working even if the autofs path is unmounted later.
fn make_links_for_snapshot(cur_mnt: &str, final_mnt: &str) -> Result<()> {
    let profile = sentryusb_vehicle_profile::Profile::active();
    make_links_in(Path::new(RECORDINGS), profile, cur_mnt, final_mnt)
}

/// [`make_links_for_snapshot`] over an explicit recordings root (testable).
fn make_links_in(
    recordings: &Path,
    profile: &sentryusb_vehicle_profile::Profile,
    cur_mnt: &str,
    final_mnt: &str,
) -> Result<()> {
    let rec_root = Path::new(cur_mnt).join(&profile.recording.root);
    info!("Making links for {}, retargeted to {}", cur_mnt, final_mnt);
    let mut made = 0usize;
    link_clips_under(&rec_root, recordings, profile, cur_mnt, final_mnt, 0, &mut made);
    info!("Made {} link(s) for {}", made, cur_mnt);
    Ok(())
}

/// Recursive walk (bounded depth — today's GM layout is flat, but a
/// firmware that starts date-bucketing must not break capture). Every
/// file whose name matches the profile's clip pattern gets one symlink
/// under `<recordings>/Continuous/<YYYY-MM-DD>/`, dated from the
/// filename timestamp — clip names carry no leading date on GM, so the
/// parsed timestamp is the only reliable bucket key.
#[cfg_attr(not(unix), allow(unused_variables))]
fn link_clips_under(
    dir: &Path,
    recordings: &Path,
    profile: &sentryusb_vehicle_profile::Profile,
    cur_mnt: &str,
    final_mnt: &str,
    depth: u8,
    made: &mut usize,
) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ftype) = entry.file_type() else { continue };
        if ftype.is_dir() {
            link_clips_under(&path, recordings, profile, cur_mnt, final_mnt, depth + 1, made);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(clip) = profile.parse_clip_filename(&name) else {
            continue;
        };
        let day_dir = recordings.join("Continuous").join(clip.date_str());
        let _ = std::fs::create_dir_all(&day_dir);
        let link = day_dir.join(&name);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let target = retarget_path(&path, cur_mnt, final_mnt);
            if std::os::unix::fs::symlink(&target, &link).is_ok() {
                *made += 1;
            }
        }
    }
}

/// Replace `cur_mnt` prefix with `final_mnt` so the symlink target
/// references the stable `<snapdir>/mnt` path rather than the autofs
/// `/tmp/snapshots/...` mount which can come and go.
#[cfg(unix)]
fn retarget_path(file: &Path, cur_mnt: &str, final_mnt: &str) -> String {
    let s = file.to_string_lossy().to_string();
    if let Some(stripped) = s.strip_prefix(cur_mnt) {
        format!("{}{}", final_mnt, stripped)
    } else {
        s
    }
}

/// Walk every completed snapshot (one with a `.toc`) and rebuild the
/// `/mutable/Recordings/...` symlinks for any whose links have gone
/// missing (post-setup-re-run recovery via the rebuild flag file).
pub fn rebuild_all_snapshot_links() -> Result<()> {
    let mut rebuilt = 0usize;
    let entries = match std::fs::read_dir(SNAPSHOTS_DIR) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let snap_dir_path = entry.path();
        if !snap_dir_path.is_dir() {
            continue;
        }
        let snap_name = entry.file_name().to_string_lossy().to_string();
        if !snap_name.starts_with("snap-") {
            continue;
        }
        let toc = snap_dir_path.join("snap.bin.toc");
        let bin = snap_dir_path.join("snap.bin");
        if !toc.exists() || !bin.exists() {
            continue;
        }
        let snap_mnt = format!("/tmp/snapshots/{}", snap_name);
        let snap_mnt_link = snap_dir_path.join("mnt");

        if !snap_mnt_link.exists() {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&snap_mnt, &snap_mnt_link);
        }

        if has_existing_links_into_snapshot(&snap_name) {
            continue;
        }

        // Verify the snapshot can mount before we ask make_links to walk it.
        if std::fs::read_dir(&snap_mnt).is_err() {
            warn!("rebuild: snapshot {} not mountable, skipping", snap_name);
            continue;
        }

        if let Err(e) = make_links_for_snapshot(
            &snap_mnt,
            &snap_mnt_link.to_string_lossy().to_string(),
        ) {
            warn!("rebuild: make_links_for_snapshot {}: {}", snap_name, e);
            continue;
        }
        rebuilt += 1;
    }

    if rebuilt > 0 {
        info!("Rebuilt symlinks for {} snapshot(s)", rebuilt);
    }
    Ok(())
}

/// Check whether any symlink under `/mutable/Recordings/` already
/// points at this snapshot. Used to skip rebuilds for snapshots that
/// are already linked.
fn has_existing_links_into_snapshot(snap_name: &str) -> bool {
    let needle = format!("/{}/", snap_name);
    walk_for_symlink_pointing_at(Path::new(RECORDINGS), &needle, 0)
}

fn walk_for_symlink_pointing_at(dir: &Path, needle: &str, depth: u8) -> bool {
    if depth > 4 {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let md = match entry.file_type() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_symlink() {
            if let Ok(t) = std::fs::read_link(&p) {
                if t.to_string_lossy().contains(needle) {
                    return true;
                }
            }
        } else if md.is_dir() {
            if walk_for_symlink_pointing_at(&p, needle, depth + 1) {
                return true;
            }
        }
    }
    false
}

/// On 32-bit Bookworm (Pi Zero/Zero2/Pi3 + 32-bit userspace) the exFAT
/// driver mis-handles atimes past Y2038, leaving snapshots unfsck-able.
/// Mount the snapshot RW, find files newer-than-2038, touch them to
/// "now", then unmount. Bash lines 292-299.
async fn apply_bookworm_32bit_timestamp_fix(snap_file: &str) -> Result<()> {
    // Bookworm = Debian VERSION_ID="12".
    let osr = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let is_bookworm = osr
        .lines()
        .any(|l| l.trim() == "VERSION_ID=\"12\"" || l.trim() == "VERSION_ID=12");
    if !is_bookworm {
        return Ok(());
    }

    let tmpmnt = sentryusb_shell::run("mktemp", &["-d"]).await?.trim().to_string();
    if tmpmnt.is_empty() {
        return Ok(());
    }
    let mount_ok = sentryusb_shell::run(
        "/root/bin/mountimage",
        &[snap_file, &tmpmnt, "rw"],
    )
    .await
    .is_ok();
    if !mount_ok {
        let _ = sentryusb_shell::run("rmdir", &[&tmpmnt]).await;
        return Ok(());
    }
    let cmd = format!(
        "find {} -newerat 20380101 -print0 | xargs -r -0 touch",
        shell_escape(&tmpmnt)
    );
    let _ = sentryusb_shell::run("bash", &["-c", &cmd]).await;
    let _ = sentryusb_shell::run("umount", &[&tmpmnt]).await;
    let _ = sentryusb_shell::run("rmdir", &[&tmpmnt]).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_bare_name() {
        // autofs and a correct WebUI call pass the bare id.
        assert_eq!(normalize_snap_name("snap-000001").as_deref(), Some("snap-000001"));
    }

    #[test]
    fn normalize_accepts_full_path() {
        // The regression: the WebUI delete handler (and make_snapshot.sh's
        // discard path) pass a full path. The old `contains('/')` guard
        // rejected this outright, so deletes failed via the thin-wrapper
        // `release_snapshot.sh` → `dashusb snapshot release "$@"` route.
        assert_eq!(
            normalize_snap_name("/backingfiles/snapshots/snap-000001").as_deref(),
            Some("snap-000001"),
        );
    }

    #[test]
    fn normalize_accepts_trailing_slash() {
        assert_eq!(
            normalize_snap_name("/backingfiles/snapshots/snap-000042/").as_deref(),
            Some("snap-000042"),
        );
    }

    #[test]
    fn normalize_rejects_non_snapshot() {
        assert_eq!(normalize_snap_name("etc"), None);
        assert_eq!(normalize_snap_name(""), None);
        assert_eq!(normalize_snap_name(".."), None);
    }

    #[test]
    fn normalize_rejects_traversal() {
        // basename takes only the final component, so traversal can't
        // escape SNAPSHOTS_DIR — the final segment isn't a `snap-` name.
        assert_eq!(normalize_snap_name("snap-1/../../etc/passwd"), None);
        assert_eq!(normalize_snap_name("/etc/../snap-1/.."), None);
    }

    /// The profile-driven link farm: GM-named clips land in dated
    /// Continuous buckets keyed by the filename timestamp, non-matching
    /// files are ignored, and targets are retargeted onto the stable
    /// `<snapdir>/mnt` path.
    #[cfg(unix)]
    #[test]
    fn links_gm_clips_into_dated_continuous_buckets() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let snap_mnt = tmp.path().join("snapmnt");
        let rec = snap_mnt.join(
            "Android/media/com.gm.ultifi.gmconnectedcameraservice/Recordings/SurroundVisionRecorder",
        );
        std::fs::create_dir_all(&rec).unwrap();
        for name in [
            "FRONT_2026_07_17_T_19_34_53.mp4",
            "REAR_2026_07_17_T_19_34_53.mp4",
            "LEFT_2026_07_18_T_08_00_00.mp4",
        ] {
            std::fs::write(rec.join(name), b"x").unwrap();
        }
        std::fs::write(rec.join("metadata.json"), b"x").unwrap();

        let profile = sentryusb_vehicle_profile::Profile::embedded("gm_surroundvision")
            .unwrap()
            .unwrap();
        let recordings = tmp.path().join("Recordings");
        let cur = snap_mnt.to_string_lossy().to_string();
        make_links_in(
            &recordings,
            &profile,
            &cur,
            "/backingfiles/snapshots/snap-000001/mnt",
        )
        .unwrap();

        let day = recordings.join("Continuous/2026-07-17");
        for name in ["FRONT_2026_07_17_T_19_34_53.mp4", "REAR_2026_07_17_T_19_34_53.mp4"] {
            assert!(day.join(name).is_symlink(), "{name} must be linked");
        }
        assert!(recordings
            .join("Continuous/2026-07-18/LEFT_2026_07_18_T_08_00_00.mp4")
            .is_symlink());
        assert!(!day.join("metadata.json").exists(), "non-clips must be skipped");

        let target = std::fs::read_link(day.join("FRONT_2026_07_17_T_19_34_53.mp4")).unwrap();
        assert!(
            target
                .to_string_lossy()
                .starts_with("/backingfiles/snapshots/snap-000001/mnt/"),
            "target must be retargeted, got {target:?}",
        );
    }
}
