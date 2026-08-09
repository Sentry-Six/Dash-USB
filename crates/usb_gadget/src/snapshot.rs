//! Reflink-backed copy-on-write captures of the cam disk image, plus the
//! bookkeeping that makes them browseable from the web UI and
//! `/mutable/Recordings/`.
//!
//! Per snapshot: `cp --reflink` the live image, optional fsck, read-only
//! mount through autofs, TOC-diff against the previous snapshot (identical
//! means discard), then one symlink per clip matching the vehicle profile
//! under `/mutable/Recordings/Continuous/<YYYY-MM-DD>/`. That link tree is
//! what archiveloop archives and the Viewer plays.
//!
//! The car rolling-deletes footage on the live drive (2 h on GM). Snapshots
//! taken inside that window are the only reason footage outlives it.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use tracing::{info, warn};

const SNAPSHOTS_DIR: &str = "/backingfiles/snapshots";
const CAM_DISK: &str = "/backingfiles/cam_disk.bin";
const REBUILD_FLAG: &str = "/mutable/.rebuild_snapshot_symlinks";

const RECORDINGS: &str = "/mutable/Recordings";

/// Serializes snapshot creation, release, and free-space pruning. Deleting
/// a snapshot mid-`make_snapshot`, or two pruners racing, corrupts the
/// TOC-diff chain.
const SNAPSHOT_MGMT_LOCK: &str = "/tmp/dashusb_snapshot_mgmt.lock";

/// Take the snapshot-management flock. Blocking; released on drop.
pub(crate) fn acquire_mgmt_lock() -> std::io::Result<super::cycle_lock::CycleGuard> {
    super::cycle_lock::acquire_path(
        Path::new(SNAPSHOT_MGMT_LOCK),
        Duration::from_secs(120),
    )
}

/// Create a snapshot of the cam disk plus the symlink and TOC bookkeeping
/// the archive loop and the Viewer need.
///
/// `skip_fsck` skips the fsck pass; archiveloop sets it after a reboot so
/// fsck does not run twice in quick succession.
///
/// Returns `Some(name)` for a fresh snapshot, or `None` when the capture is
/// byte-equivalent to the previous one, in which case the reflink is
/// deleted rather than accumulating identical copies.
pub async fn make_snapshot(skip_fsck: bool) -> Result<Option<String>> {
    let _mgmt = acquire_mgmt_lock()?;
    let _ = std::fs::create_dir_all(SNAPSHOTS_DIR);

    if !Path::new(CAM_DISK).exists() {
        bail!("cam disk image not found at {}", CAM_DISK);
    }

    // No `.toc` on the previous snapshot means it was abandoned
    // mid-flight: wipe it and reuse the slot.
    let (snap_num, prev_toc) = pick_next_snapshot_slot()?;
    let snap_name = format!("snap-{:06}", snap_num);
    let snap_dir = format!("{}/{}", SNAPSHOTS_DIR, snap_name);
    let snap_file = format!("{}/snap.bin", snap_dir);
    let snap_mnt = format!("/tmp/snapshots/{}", snap_name);
    let snap_mnt_link = format!("{}/mnt", snap_dir);

    std::fs::create_dir_all(&snap_dir)?;
    info!("Taking snapshot of cam disk in {}", snap_dir);

    // `--reflink=auto` keeps non-XFS backing stores working at the cost of
    // a full copy. The setup wizard's XFS check usually catches those first.
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

    if !skip_fsck {
        if let Err(e) = fsck_snapshot(&snap_file).await {
            warn!("fsck on {} failed (non-fatal): {}", snap_file, e);
        }
    }

    // Symlinks resolve through /tmp/snapshots/..., the autofs mount root.
    // autofs must be active first or `find` below sees an empty dir.
    wait_for_autofs().await;

    info!("Took snapshot {}", snap_name);

    // Touch the autofs path first so the image is mounted before `find`
    // traverses it.
    let _ = sentryusb_shell::run("ls", &[&format!("{}/", snap_mnt)]).await;

    let toc_path = format!("{}.toc", snap_file);
    let toc_path_tmp = format!("{}_", toc_path);
    if let Err(e) = generate_toc(&snap_mnt, &toc_path_tmp).await {
        warn!("toc generation failed for {}: {}", snap_mnt, e);
    }

    // Nothing new versus the prior TOC means this is a duplicate: release
    // it and return None so callers don't think they got a fresh snapshot.
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
    // Deletions are NEVER mirrored into the snapshot link trees.
    // Preserving footage past the car's window is the whole product.

    // Pre-create <snapdir>/mnt. Per-clip symlinks target
    // <snapdir>/mnt/<recording root>/..., and without it they resolve to
    // nothing until autofs is poked.
    if !Path::new(&snap_mnt_link).exists() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&snap_mnt, &snap_mnt_link);
    }

    // Previous snapshot's TOC feeds the newest-per-camera stability check
    // (see make_links_in).
    let prev_sizes = prev_toc.as_deref().map(parse_toc_sizes);
    if let Err(e) = make_links_for_snapshot(&snap_mnt, &snap_mnt_link, prev_sizes.as_ref()) {
        warn!("make_links_for_snapshot failed: {}", e);
    }

    // This rename is the commit point: slot reuse and free-space pruning
    // both treat a snapshot without a `.toc` as abandoned.
    let _ = std::fs::rename(&toc_path_tmp, &toc_path);

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
/// e.g. the WebUI delete handler and `make_snapshot.sh`'s discard path), so
/// only the final path component is used. That also neutralizes `..`
/// traversal: the last component is all that ever gets appended to
/// `SNAPSHOTS_DIR`.
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
    let _mgmt = acquire_mgmt_lock()?;
    release_snapshot_unlocked(snap_name).await
}

/// [`release_snapshot`] body without the management lock, for callers that
/// already hold it (the space manager). flock is per-fd, so
/// re-acquiring from the same process would deadlock against ourselves.
pub(crate) async fn release_snapshot_unlocked(snap_name: &str) -> Result<()> {
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
    // Drop every /mutable/Recordings symlink that pointed into this
    // snapshot, then prune the date folders left empty. Without this,
    // released snapshots leave broken links that clutter the Viewer and
    // Samba and slowly eat mutable's inodes.
    prune_links_into(&name);
    info!("Released snapshot: {}", name);
    Ok(())
}

/// Remove symlinks under `/mutable/Recordings` targeting `snap_name`,
/// then delete any directories left empty.
fn prune_links_into(snap_name: &str) {
    let needle = format!("/{}/", snap_name);
    fn walk(dir: &Path, needle: &str, depth: u8) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ftype) = entry.file_type() else { continue };
            if ftype.is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    if target.to_string_lossy().contains(needle) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            } else if ftype.is_dir() {
                walk(&path, needle, depth + 1);
                // Remove if pruning emptied it (fails harmlessly otherwise).
                let _ = std::fs::remove_dir(&path);
            }
        }
    }
    walk(Path::new(RECORDINGS), &needle, 0);
}

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

/// Find the next free `snap-NNNNNN` slot. A previous snapshot with no
/// `.toc` (or no snap.bin) was interrupted mid-run, so its number is
/// wiped and reused.
///
/// Returns `(snap_num, Option<previous_toc_path>)`. The previous TOC
/// is `None` on a brand-new install (no completed snapshots yet).
/// True when anything is mounted inside `<SNAPSHOTS_DIR>/<name>` or at that
/// snapshot's autofs mount (`/tmp/snapshots/<name>`). Prefix match: the loop
/// image mounts *under* the snapshot dir, so an exact compare would miss it.
fn snapshot_slot_has_mounts(name: &str) -> bool {
    let under = format!("{}/{}/", SNAPSHOTS_DIR, name);
    let autofs = format!("/tmp/snapshots/{}", name);
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        // Can't prove it's unmounted → assume it is mounted and skip the
        // destructive reuse. Losing one slot number is free; wiping a live
        // mount is not.
        return true;
    };
    mounts.lines().any(|l| {
        let Some(mp) = l.split_whitespace().nth(1) else {
            return false;
        };
        mp.starts_with(&under) || mp == autofs || mp.starts_with(&format!("{}/", autofs))
    })
}

fn pick_next_snapshot_slot() -> Result<(u32, Option<String>)> {
    // Option, not a 0 sentinel: "only snap-000000 exists" used to be
    // indistinguishable from "no snapshots at all", which skipped the
    // identical-snapshot TOC compare against a real snap-000000.
    let mut max_num: Option<u32> = None;
    if let Ok(entries) = std::fs::read_dir(SNAPSHOTS_DIR) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let num = name
                .strip_prefix("snap-")
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u32>().ok());
            // Numeric-only, and file_type() does not follow symlinks: a
            // stray `snap-000999` FILE or a planted link must not drive slot
            // allocation. It would skip the counter ahead and break the TOC
            // backstop chain, since the "previous" slot it points at holds
            // no snapshot.
            if let Some(num) = num
                && entry.file_type().is_ok_and(|ft| ft.is_dir())
                && max_num.is_none_or(|m| num > m)
            {
                max_num = Some(num);
            }
        }
    }

    let Some(max_num) = max_num else {
        return Ok((1, None));
    };

    let prev_name = format!("snap-{:06}", max_num);
    let prev_dir = format!("{}/{}", SNAPSHOTS_DIR, prev_name);
    let prev_toc = format!("{}/snap.bin.toc", prev_dir);
    let prev_bin = format!("{}/snap.bin", prev_dir);

    // Look one slot further back for a usable previous TOC. `> 0` so a store
    // whose only completed snapshot is snap-000000 still yields a backstop.
    let backstop_before = |n: u32| -> Option<String> {
        if n > 0 {
            let p = format!("{}/snap-{:06}/snap.bin.toc", SNAPSHOTS_DIR, n - 1);
            if Path::new(&p).exists() { Some(p) } else { None }
        } else {
            None
        }
    };

    // Abandoned: no TOC was committed → reuse this slot.
    if !Path::new(&prev_toc).exists() || !Path::new(&prev_bin).exists() {
        // ...unless something is still mounted under it (a stuck autofs
        // mount, or a crash mid-snapshot while the loop image is live).
        // `remove_dir_all` would race a live mount and could tear down
        // footage the archive is still reading, so append past it instead.
        if snapshot_slot_has_mounts(&prev_name) {
            let next = max_num + 1;
            warn!(
                "slot pick: max_seen={} incomplete BUT MOUNTED — appending next={}",
                max_num, next
            );
            return Ok((next, backstop_before(max_num)));
        }
        let _ = std::fs::remove_dir_all(&prev_dir);
        info!(
            "slot pick: max_seen={} action=reuse-incomplete next={}",
            max_num, max_num
        );
        return Ok((max_num, backstop_before(max_num)));
    }

    Ok((max_num + 1, Some(prev_toc)))
}

/// fsck the snapshot's filesystem partition via a temporary loop device.
/// Failures are logged but non-fatal so `archive-clips` still runs: losing
/// strict verification of one snapshot beats aborting the whole archive
/// cycle.
async fn fsck_snapshot(snap_file: &str) -> Result<()> {
    let loop_dev = losetup_find_show(snap_file).await?;
    let part = format!("{}p1", loop_dev);

    // `-p` works for both vfat and exfat. The exit status is discarded
    // deliberately; see the doc comment.
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(120),
        "fsck",
        &[&part, "--", "-p"],
    )
    .await;

    let _ = sentryusb_shell::run("losetup", &["-d", &loop_dev]).await;
    Ok(())
}

/// `losetup -f -P --show <file>` with a retry loop: some kernels race on
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

/// Wait for autofs to become active, capped at 30 retries (~30s) so a
/// misconfigured system doesn't hang the archive cycle indefinitely.
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

/// Write a TOC to `out_path`: one `<size> <relative-path>` line per file
/// under `root`.
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
    // Single-quote, escaping any embedded quote.
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse a TOC (`<size> <path>` per line, `find -printf '%s %P'`) into
/// a relative-path → size map for the stability check.
fn parse_toc_sizes(toc_path: &str) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    for line in std::fs::read_to_string(toc_path).unwrap_or_default().lines() {
        if let Some((size, path)) = line.split_once(' ') {
            if let Ok(size) = size.parse::<u64>() {
                map.insert(path.to_string(), size);
            }
        }
    }
    map
}

/// Returns true if `new_toc` has any line that isn't in `old_toc`.
/// Lines are `<size> <path>`, compared whole: a clip that merely grew
/// (same name, new size, e.g. still being written during the
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
/// the scan). `final_mnt` is `<snapdir>/mnt`, the symlink to the autofs
/// path. Per-clip symlinks are retargeted onto `final_mnt` so they keep
/// working even if the autofs path is unmounted later.
fn make_links_for_snapshot(
    cur_mnt: &str,
    final_mnt: &str,
    prev_sizes: Option<&std::collections::HashMap<String, u64>>,
) -> Result<()> {
    let profile = sentryusb_vehicle_profile::Profile::active();
    make_links_in(Path::new(RECORDINGS), profile, cur_mnt, final_mnt, prev_sizes)
}

/// [`make_links_for_snapshot`] over an explicit recordings root (testable).
///
/// Closed-segment guard, so a segment the car is still writing is never
/// archived (a truncated offsite copy would poison the dedup ledger).
/// Per camera, every clip EXCEPT the newest links immediately: a successor
/// proves the predecessor closed, and that holds without reading the car's
/// clock. The newest clip per camera links only once its size is unchanged
/// versus the previous snapshot's TOC (snapshots are ≥ the segment length
/// apart by default, so an equal size means closed). With no previous TOC
/// the newest clip is held one cycle; the snapshot itself still preserves
/// the bytes.
fn make_links_in(
    recordings: &Path,
    profile: &sentryusb_vehicle_profile::Profile,
    cur_mnt: &str,
    final_mnt: &str,
    prev_sizes: Option<&std::collections::HashMap<String, u64>>,
) -> Result<()> {
    let rec_root = Path::new(cur_mnt).join(&profile.recording.root);
    info!("Making links for {}, retargeted to {}", cur_mnt, final_mnt);

    let mut clips = Vec::new();
    collect_clips_under(&rec_root, profile, cur_mnt, 0, &mut clips);

    // Newest timestamp per camera in THIS snapshot.
    let mut newest: std::collections::HashMap<&str, chrono::NaiveDateTime> =
        std::collections::HashMap::new();
    for (_, _, clip, _) in &clips {
        newest
            .entry(clip.camera.as_str())
            .and_modify(|t| {
                if clip.timestamp > *t {
                    *t = clip.timestamp;
                }
            })
            .or_insert(clip.timestamp);
    }

    let mut made = 0usize;
    let mut held = 0usize;
    for (path, name, clip, size) in &clips {
        let is_newest = newest.get(clip.camera.as_str()) == Some(&clip.timestamp);
        if is_newest {
            // Relative path as the TOC records it (`find -printf '%P'`).
            let rel = path
                .strip_prefix(cur_mnt)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let stable = prev_sizes
                .and_then(|m| m.get(&rel))
                .is_some_and(|prev| *prev == *size);
            if !stable {
                held += 1;
                continue;
            }
        }
        let day_dir = recordings.join("Continuous").join(clip.date_str());
        let _ = std::fs::create_dir_all(&day_dir);
        let link = day_dir.join(name);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        {
            let target = retarget_path(path, cur_mnt, final_mnt);
            if std::os::unix::fs::symlink(&target, &link).is_ok() {
                made += 1;
            }
        }
    }
    info!(
        "Made {} link(s) for {} ({} newest-per-camera held until size-stable)",
        made, cur_mnt, held
    );
    Ok(())
}

/// Collect every file under `dir` whose name matches the profile's clip
/// pattern; linking decisions happen in [`make_links_in`]. The walk is
/// depth-bounded: today's GM layout is flat, but a firmware that starts
/// date-bucketing must not break capture.
fn collect_clips_under(
    dir: &Path,
    profile: &sentryusb_vehicle_profile::Profile,
    cur_mnt: &str,
    depth: u8,
    out: &mut Vec<(std::path::PathBuf, String, sentryusb_vehicle_profile::ClipInfo, u64)>,
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
            collect_clips_under(&path, profile, cur_mnt, depth + 1, out);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(clip) = profile.parse_clip_filename(&name) else {
            continue;
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((path, name, clip, size));
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

        // Confirm the snapshot mounts before walking it.
        if std::fs::read_dir(&snap_mnt).is_err() {
            warn!("rebuild: snapshot {} not mountable, skipping", snap_name);
            continue;
        }

        if let Err(e) = make_links_for_snapshot(
            &snap_mnt,
            &snap_mnt_link.to_string_lossy().to_string(),
            None,
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

/// Whether any symlink under `/mutable/Recordings/` already points at this
/// snapshot, which means its rebuild can be skipped.
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
        // The WebUI delete handler and make_snapshot.sh's discard path
        // pass a full path; a `contains('/')` guard here broke deletes.
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
        // escape SNAPSHOTS_DIR: the final segment isn't a `snap-` name.
        assert_eq!(normalize_snap_name("snap-1/../../etc/passwd"), None);
        assert_eq!(normalize_snap_name("/etc/../snap-1/.."), None);
    }

    /// The profile-driven link farm with the closed-segment guard:
    /// per camera, non-newest clips link immediately (a successor
    /// proves the predecessor closed); the newest clip links only when
    /// its size matches the previous snapshot's TOC.
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
        // FRONT has two segments (old + newest); LEFT has one (newest only).
        for name in [
            "FRONT_2026_07_17_T_19_34_53.mp4",
            "FRONT_2026_07_17_T_19_39_53.mp4",
            "LEFT_2026_07_18_T_08_00_00.mp4",
        ] {
            std::fs::write(rec.join(name), b"xxxx").unwrap();
        }
        std::fs::write(rec.join("metadata.json"), b"x").unwrap();

        let profile = sentryusb_vehicle_profile::Profile::embedded("gm_surroundvision")
            .unwrap()
            .unwrap();
        let recordings = tmp.path().join("Recordings");
        let cur = snap_mnt.to_string_lossy().to_string();

        // No previous TOC: only the non-newest FRONT clip may link.
        make_links_in(&recordings, &profile, &cur, "/backingfiles/snapshots/snap-000001/mnt", None)
            .unwrap();
        let day = recordings.join("Continuous/2026-07-17");
        assert!(day.join("FRONT_2026_07_17_T_19_34_53.mp4").is_symlink());
        assert!(
            !day.join("FRONT_2026_07_17_T_19_39_53.mp4").exists(),
            "newest FRONT must be held without a stability proof"
        );
        assert!(
            !recordings.join("Continuous/2026-07-18/LEFT_2026_07_18_T_08_00_00.mp4").exists(),
            "sole LEFT clip is newest — held"
        );
        assert!(!day.join("metadata.json").exists(), "non-clips must be skipped");
        let target = std::fs::read_link(day.join("FRONT_2026_07_17_T_19_34_53.mp4")).unwrap();
        assert!(
            target.to_string_lossy().starts_with("/backingfiles/snapshots/snap-000001/mnt/"),
            "target must be retargeted, got {target:?}",
        );

        // Previous TOC says the newest clips already had these sizes →
        // stable → they link now.
        let mut prev = std::collections::HashMap::new();
        let root = "Android/media/com.gm.ultifi.gmconnectedcameraservice/Recordings/SurroundVisionRecorder";
        prev.insert(format!("{root}/FRONT_2026_07_17_T_19_39_53.mp4"), 4u64);
        prev.insert(format!("{root}/LEFT_2026_07_18_T_08_00_00.mp4"), 4u64);
        make_links_in(
            &recordings,
            &profile,
            &cur,
            "/backingfiles/snapshots/snap-000002/mnt",
            Some(&prev),
        )
        .unwrap();
        assert!(day.join("FRONT_2026_07_17_T_19_39_53.mp4").is_symlink());
        assert!(recordings
            .join("Continuous/2026-07-18/LEFT_2026_07_18_T_08_00_00.mp4")
            .is_symlink());

        // A newest clip whose size GREW since the previous TOC stays held.
        std::fs::write(rec.join("REAR_2026_07_17_T_19_39_53.mp4"), b"xxxxxxxx").unwrap();
        let mut grew = std::collections::HashMap::new();
        grew.insert(format!("{root}/REAR_2026_07_17_T_19_39_53.mp4"), 4u64);
        make_links_in(
            &recordings,
            &profile,
            &cur,
            "/backingfiles/snapshots/snap-000003/mnt",
            Some(&grew),
        )
        .unwrap();
        assert!(
            !day.join("REAR_2026_07_17_T_19_39_53.mp4").exists(),
            "size changed since last snapshot — still recording, must hold"
        );
    }

    /// TOC parsing: `<size> <path>` lines → map.
    #[test]
    fn parses_toc_sizes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let toc = tmp.path().join("snap.bin.toc");
        std::fs::write(&toc, "111 a/b/FRONT_x.mp4
not-a-line
222 c.mp4
").unwrap();
        let m = parse_toc_sizes(&toc.to_string_lossy());
        assert_eq!(m.get("a/b/FRONT_x.mp4"), Some(&111));
        assert_eq!(m.get("c.mp4"), Some(&222));
        assert_eq!(m.len(), 2);
    }
}
