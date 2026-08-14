//! Guarded XFS repair for an external `/backingfiles` device.
//! Root disks are excluded, the web service remains running, and destructive
//! `xfs_repair -L` requires explicit authorization. Success requires reboot.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::router::AppState;

const BACKINGFILES: &str = "/backingfiles";
/// XFS label used to resolve an unmounted backingfiles partition.
const XFS_LABEL: &str = "backingfiles";
/// Bind/autofs mount points archiveloop exposes from inside `/backingfiles`.
/// All must be released before the device can be unmounted.
const SUBMOUNTS: &[&str] = &["/mnt/cam"];
/// Writable partition where the human-readable repair transcript is kept.
const REPAIR_LOG_DIR: &str = "/mutable";
/// Five-minute ceiling for repair on multi-terabyte media.
const CMD_TIMEOUT: Duration = Duration::from_secs(300);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

// Command capture

/// Outcome of a spawned command with its combined output preserved
/// regardless of exit status.
struct CmdResult {
    ok: bool,
    code: i32,
    output: String,
}

/// Capture output and status because `xfs_repair -n` uses nonzero for damage.
async fn run_capture(timeout: Duration, name: &str, args: &[&str]) -> CmdResult {
    let fut = Command::new(name).args(args).kill_on_drop(true).output();
    match tokio::time::timeout(timeout, fut).await {
        Err(_) => CmdResult {
            ok: false,
            code: -1,
            output: format!("(timed out after {timeout:?})"),
        },
        Ok(Err(e)) => CmdResult {
            ok: false,
            code: -1,
            output: format!("(failed to spawn {name}: {e})"),
        },
        Ok(Ok(o)) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(&err);
            }
            CmdResult {
                ok: o.status.success(),
                code: o.status.code().unwrap_or(-1),
                output: s.trim_end().to_string(),
            }
        }
    }
}

// Pure helpers (unit-tested)

/// Strip the partition suffix from a `/dev` name to get the parent disk.
/// `sda2` → `sda`, `mmcblk0p2` → `mmcblk0`, `nvme0n1p3` → `nvme0n1`.
/// Mirrors the parent-disk logic in [`crate::devices`].
fn parent_disk(dev: &str) -> String {
    let d = dev.strip_prefix("/dev/").unwrap_or(dev);
    if d.contains("mmcblk") || d.contains("nvme") || d.contains("loop") {
        // p-separated partition suffix, e.g. mmcblk0p2 / nvme0n1p3.
        if let Some(idx) = d.rfind('p') {
            let suffix = &d[idx + 1..];
            if idx > 0 && !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return d[..idx].to_string();
            }
        }
        d.to_string()
    } else {
        // sd-style: partition suffix is trailing digits.
        let trimmed: String = d
            .chars()
            .rev()
            .skip_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if trimmed.is_empty() { d.to_string() } else { trimmed }
    }
}

/// Find the source device for a mountpoint in `/proc/mounts` text.
fn resolve_mount_source(mounts: &str, mountpoint: &str) -> Option<String> {
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(src), Some(mp)) = (f.next(), f.next()) else {
            continue;
        };
        if mp == mountpoint && src.starts_with("/dev/") {
            return Some(src.to_string());
        }
    }
    None
}

/// True when `xfs_repair` output says the dirty log must be replayed (or
/// destroyed with `-L`). These phrases are stable across xfs_repair versions.
fn needs_log_replay(out: &str) -> bool {
    let l = out.to_ascii_lowercase();
    l.contains("destroy the log") || l.contains("metadata changes in a log")
}

/// Persistent incident marker written before auto-repair to prevent reboot loops.
const AUTO_REPAIR_MARKER: &str = "/mutable/.storage_auto_repair_attempted";

fn marker_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

fn write_marker(path: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Err(e) = std::fs::write(path, format!("{{\"ts\":{ts}}}\n")) {
        tracing::warn!("[storage-boot] failed to write marker {path}: {e}");
    }
}

fn clear_marker(path: &str) {
    let _ = std::fs::remove_file(path);
}

/// Boot action after a one-shot mount retry and incident-marker check.
#[derive(Debug, PartialEq, Eq)]
enum BootAction {
    /// Feature off or storage not eligible: do nothing.
    Skip(&'static str),
    /// Storage healthy: clear any stale incident marker.
    ClearMarker,
    /// Corrupt again after a previous auto attempt: notify, never loop.
    NotifyRepeatCorruption,
    /// First detection of this incident: run the auto repair.
    Repair,
}

fn decide_boot_action(
    auto_enabled: bool,
    device_found: bool,
    external: bool,
    mounted: bool,
    marker_present: bool,
) -> BootAction {
    if !auto_enabled {
        return BootAction::Skip("storage_auto_repair disabled");
    }
    if !device_found {
        return BootAction::Skip("no backingfiles device found");
    }
    if !external {
        return BootAction::Skip("backingfiles not on an external drive");
    }
    if mounted {
        return BootAction::ClearMarker;
    }
    if marker_present {
        return BootAction::NotifyRepeatCorruption;
    }
    BootAction::Repair
}

/// Escalation policy: interactive mode allows `-L` only for a confirmed dirty
/// log; auto mode allows it after any failure only when pre-authorized.
#[derive(Debug, PartialEq, Eq)]
enum Escalation {
    /// Repair succeeded; continue to verification.
    Proceed,
    /// Stop; a destructive repair is required but not authorized.
    StopNeedsForce,
    /// Run `xfs_repair -L` now.
    RunForce,
    /// Unrepairable by anything `-L` could fix (interactive only).
    Fail,
}

fn escalation_action(rep_ok: bool, needs_replay: bool, auto: bool, force_allowed: bool) -> Escalation {
    if rep_ok {
        return Escalation::Proceed;
    }
    if auto {
        if force_allowed { Escalation::RunForce } else { Escalation::StopNeedsForce }
    } else if needs_replay {
        if force_allowed { Escalation::RunForce } else { Escalation::StopNeedsForce }
    } else {
        Escalation::Fail
    }
}

/// How a repair run terminates and who authorizes `-L`.
#[derive(Clone, Copy)]
pub(crate) enum RepairMode {
    /// Web UI requires explicit `-L` confirmation and a manual reboot.
    Interactive { confirm_destructive: bool },
    /// Boot mode can use pre-authorization and reboot after success.
    AutoBoot { force_allowed: bool },
}

// Auto-repair notification copy: spec-fixed wording, do not edit.
const MSG_AUTO_SUCCESS: &str = "Backing-files corruption detected at boot. Automatic repair succeeded — rebooting the Pi now.";
const MSG_NEEDS_MANUAL_FORCE: &str = "Backing-files corruption detected at boot. Automatic repair failed — you must run the force fix manually from Settings → System → Repair Storage.";
const MSG_FORCE_SUCCESS: &str = "Backing-files corruption detected at boot. Force fix succeeded after the regular repair failed — rebooting the Pi now.";
const MSG_HARD_FAIL: &str = "Backing-files corruption detected at boot. Automatic repair FAILED — the drive may be failing. Check the SSD's power, cable and enclosure.";
const MSG_REPEAT_CORRUPTION: &str = "Backing-files corruption detected again after a recent auto repair. Not retrying automatically — the SSD may be failing. Check power/cable/enclosure and run repair manually.";

/// Title for storage-repair push notifications: `$NOTIFICATION_TITLE` from
/// dashusb.conf, falling back to "DashUSB" like the runtime scripts do.
fn notification_title() -> String {
    let (active, _) = sentryusb_config::parse_file(sentryusb_config::find_config_path())
        .unwrap_or_default();
    active
        .get("NOTIFICATION_TITLE")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "DashUSB".to_string())
}

/// Fire-and-record a storage_repair push (all configured channels +
/// notification history). Best-effort; failures only log.
async fn notify_storage_repair(message: &str) {
    let title = notification_title();
    if crate::notifications::dispatch_and_record(&title, message, Some("storage_repair"), None, None)
        .await
        .is_none()
    {
        tracing::info!("[storage-boot] notification suppressed (storage_repair type disabled)");
    }
}

// Boot-time auto repair

/// Settle delay for fstab mounts and device enumeration.
const BOOT_CHECK_DELAY: Duration = Duration::from_secs(20);

/// Called once from the server binary's startup path.
pub fn spawn_boot_check(hub: sentryusb_ws::Hub) {
    tokio::spawn(async move { boot_check(hub, AUTO_REPAIR_MARKER).await });
}

/// Auto-repair an enabled, still-unmounted backingfiles device after one mount retry.
async fn boot_check(hub: sentryusb_ws::Hub, marker: &str) {
    tokio::time::sleep(BOOT_CHECK_DELAY).await;

    let prefs = crate::preferences::load_prefs();
    let auto_enabled = crate::notification_center::bool_pref(&prefs, "storage_auto_repair", false);
    let force_allowed =
        crate::notification_center::bool_pref(&prefs, "storage_auto_force_repair", false);

    let device = resolve_backing_device().await;
    let external = match &device {
        Some(d) => is_external(d).await,
        None => false,
    };
    let mut mounted = resolve_mount_source(&read_proc_mounts().await, BACKINGFILES).is_some();

    // Distinguish corruption from late device enumeration.
    if auto_enabled && external && !mounted {
        if let Some(d) = &device {
            let r = run_capture(CMD_TIMEOUT, "mount", &[d.as_str(), BACKINGFILES]).await;
            if r.ok {
                tracing::info!(
                    "[storage-boot] /backingfiles mounted on retry — filesystem is fine, no repair"
                );
                mounted = true;
            }
        }
    }

    match decide_boot_action(auto_enabled, device.is_some(), external, mounted, marker_exists(marker)) {
        BootAction::Skip(reason) => {
            tracing::info!("[storage-boot] auto repair skipped: {reason}");
        }
        BootAction::ClearMarker => {
            clear_marker(marker);
        }
        BootAction::NotifyRepeatCorruption => {
            tracing::warn!(
                "[storage-boot] corrupt again after a previous auto repair — not retrying (marker {marker})"
            );
            notify_storage_repair(MSG_REPEAT_CORRUPTION).await;
        }
        BootAction::Repair => {
            let device = device.expect("BootAction::Repair implies device_found");
            // Repeat the HTTP handler's root-disk exclusion.
            if let Some(rp) = root_disk().await {
                if parent_disk(&device) == rp {
                    tracing::error!(
                        "[storage-boot] refusing auto repair: {device} resolves to the root disk"
                    );
                    return;
                }
            }
            write_marker(marker);
            tracing::warn!(
                "[storage-boot] /backingfiles corrupt — starting auto repair (force_allowed={force_allowed})"
            );
            run_repair(hub, device, RepairMode::AutoBoot { force_allowed }).await;
        }
    }
}

// Runtime resolution

fn canonicalize_dev(src: &str) -> String {
    std::fs::canonicalize(src)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| src.to_string())
}

async fn read_proc_mounts() -> String {
    tokio::fs::read_to_string("/proc/mounts").await.unwrap_or_default()
}

/// Parent disk of the root filesystem (`/`), e.g. `mmcblk0`.
async fn root_disk() -> Option<String> {
    let mounts = read_proc_mounts().await;
    let src = resolve_mount_source(&mounts, "/")?;
    Some(parent_disk(&canonicalize_dev(&src)))
}

/// Resolve backingfiles from its live mount, then its XFS label.
async fn resolve_backing_device() -> Option<String> {
    let mounts = read_proc_mounts().await;
    if let Some(src) = resolve_mount_source(&mounts, BACKINGFILES) {
        return Some(canonicalize_dev(&src));
    }
    // Resolve an unmounted/corrupt filesystem by label.
    let r = run_capture(PROBE_TIMEOUT, "blkid", &["-L", XFS_LABEL]).await;
    let dev = r.output.trim();
    if r.ok && dev.starts_with("/dev/") {
        return Some(canonicalize_dev(dev));
    }
    // Fall back to an lsblk label scan.
    let r = run_capture(PROBE_TIMEOUT, "lsblk", &["-rno", "PATH,LABEL"]).await;
    for line in r.output.lines() {
        let mut f = line.split_whitespace();
        if let (Some(path), Some(label)) = (f.next(), f.next()) {
            if label == XFS_LABEL && path.starts_with("/dev/") {
                return Some(path.to_string());
            }
        }
    }
    None
}

async fn device_fstype(dev: &str) -> Option<String> {
    let r = run_capture(PROBE_TIMEOUT, "lsblk", &["-rno", "FSTYPE", dev]).await;
    let t = r.output.lines().next().unwrap_or("").trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// True when `/backingfiles` is on a different physical disk than root and is
/// not the onboard SD slot. Precondition for offering repair at all.
async fn is_external(dev: &str) -> bool {
    let bp = parent_disk(dev);
    if bp == "mmcblk0" {
        return false;
    }
    match root_disk().await {
        Some(rp) => bp != rp,
        None => true,
    }
}

/// Match genuine device-specific XFS errors, excluding mount lifecycle chatter.
fn is_xfs_error_line(line: &str, devbase: &str) -> bool {
    let l = line.to_ascii_lowercase();
    if !l.contains("xfs") || !l.contains(devbase) {
        return false;
    }
    // Normal lifecycle chatter, never an error.
    if l.contains("mounting")
        || l.contains("unmounting")
        || l.contains("ending clean mount")
        || l.contains("ending clean unmount")
    {
        return false;
    }
    l.contains("error")
        || l.contains("corrupt")
        || l.contains("shut down")
        || l.contains("shutdown")
        || l.contains("i/o error")
        || l.contains("log recovery")
        || l.contains("inconsistent")
        || l.contains("needs repair")
        || l.contains("metadata corruption")
}

/// Recent genuine XFS error lines for the device, newest last.
async fn recent_xfs_errors(dev: &str) -> Vec<String> {
    let r = run_capture(PROBE_TIMEOUT, "dmesg", &["--ctime"]).await;
    let devbase = dev.strip_prefix("/dev/").unwrap_or(dev).to_ascii_lowercase();
    let mut out: Vec<String> = r
        .output
        .lines()
        .filter(|l| is_xfs_error_line(l, &devbase))
        .map(|s| s.trim().to_string())
        .collect();
    let len = out.len();
    if len > 12 {
        out = out.split_off(len - 12);
    }
    out
}

fn cam_disk_present() -> bool {
    std::path::Path::new(&format!("{BACKINGFILES}/cam_disk.bin")).exists()
}

// Persisted transcript

fn persist_log(buf: &str) -> Option<String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!("storage_repair_{ts}.log");
    let path = format!("{REPAIR_LOG_DIR}/{name}");
    match std::fs::write(&path, buf) {
        Ok(_) => Some(name),
        Err(e) => {
            tracing::warn!("failed to write repair log {path}: {e}");
            None
        }
    }
}

fn latest_repair_log() -> Option<String> {
    let rd = std::fs::read_dir(REPAIR_LOG_DIR).ok()?;
    let mut best: Option<(u64, String)> = None;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(ts) = n
            .strip_prefix("storage_repair_")
            .and_then(|x| x.strip_suffix(".log"))
            .and_then(|x| x.parse::<u64>().ok())
        {
            if best.as_ref().map_or(true, |(b, _)| ts > *b) {
                best = Some((ts, n));
            }
        }
    }
    best.map(|(_, n)| n)
}

// GET /api/storage/health

#[derive(Serialize)]
struct StorageHealth {
    /// `healthy` | `unmounted` | `corrupt` | `missing_images` | `no_external`
    state: String,
    /// Whether `/backingfiles` is on a separate external drive (gates the UI).
    external: bool,
    device: Option<String>,
    fstype: Option<String>,
    mounted: bool,
    mountpoint: String,
    cam_disk_present: bool,
    /// Recent XFS kernel errors mentioning the device, newest last.
    dmesg_errors: Vec<String>,
    /// Filename of the most recent persisted repair transcript, if any.
    last_repair_log: Option<String>,
}

pub async fn storage_health(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let device = resolve_backing_device().await;
    let mounts = read_proc_mounts().await;
    let mounted = resolve_mount_source(&mounts, BACKINGFILES).is_some();
    let external = match &device {
        Some(d) => is_external(d).await,
        None => false,
    };
    let fstype = match &device {
        Some(d) => device_fstype(d).await,
        None => None,
    };
    let cam = cam_disk_present();
    let dmesg_errors = if external {
        match &device {
            Some(d) => recent_xfs_errors(d).await,
            None => vec![],
        }
    } else {
        vec![]
    };

    let state = if !external {
        "no_external"
    } else if mounted {
        if cam { "healthy" } else { "missing_images" }
    } else if !dmesg_errors.is_empty() {
        "corrupt"
    } else {
        "unmounted"
    };

    let health = StorageHealth {
        state: state.to_string(),
        external,
        device,
        fstype,
        mounted,
        mountpoint: BACKINGFILES.to_string(),
        cam_disk_present: cam,
        dmesg_errors,
        last_repair_log: latest_repair_log(),
    };
    (
        StatusCode::OK,
        Json(serde_json::to_value(health).unwrap_or_else(|_| serde_json::json!({}))),
    )
}

// POST /api/storage/repair

#[derive(Deserialize, Default)]
struct RepairRequest {
    /// Authorizes the destructive `xfs_repair -L` last resort. Without it the
    /// flow hard-stops at the escalation gate and broadcasts `needs_force`.
    #[serde(default)]
    confirm_destructive: bool,
}

/// Broadcasts each repair step over the `storage_repair` WS channel and
/// accumulates the full transcript for persistence.
struct RepairLog {
    hub: sentryusb_ws::Hub,
    buf: String,
}

impl RepairLog {
    fn line(&mut self, phase: &str, line: impl Into<String>) {
        let line = line.into();
        self.buf.push_str(&line);
        self.buf.push('\n');
        self.hub.broadcast(
            "storage_repair",
            &serde_json::json!({ "status": "running", "phase": phase, "line": line }),
        );
    }

    fn cmd(&mut self, phase: &str, label: &str, r: &CmdResult) {
        self.line(phase, format!("$ {label}"));
        for l in r.output.lines() {
            self.line(phase, format!("  {l}"));
        }
        self.line(phase, format!("  → exit {}", r.code));
    }
}

/// Validate preconditions synchronously, then stream repair progress over WS.
pub async fn storage_repair(State(s): State<AppState>, body: String) -> (StatusCode, Json<serde_json::Value>) {
    let req: RepairRequest = serde_json::from_str(&body).unwrap_or_default();

    let device = match resolve_backing_device().await {
        Some(d) => d,
        None => {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                "Could not find the camera storage device (no /backingfiles mount and no 'backingfiles'-labelled partition).",
            )
        }
    };
    if !is_external(&device).await {
        return crate::json_error(
            StatusCode::BAD_REQUEST,
            "Storage repair is only available when camera storage is on a separate external drive.",
        );
    }
    // Repeat the eligibility check's root-disk exclusion.
    if let Some(rp) = root_disk().await {
        if parent_disk(&device) == rp {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                "Refusing to repair: the resolved device is the system/root disk.",
            );
        }
    }
    if let Some(fs) = device_fstype(&device).await {
        if fs != "xfs" {
            return crate::json_error(
                StatusCode::BAD_REQUEST,
                &format!("Storage repair currently supports XFS only (found '{fs}')."),
            );
        }
    }

    let hub = s.hub.clone();
    tokio::spawn(async move {
        run_repair(
            hub,
            device,
            RepairMode::Interactive { confirm_destructive: req.confirm_destructive },
        )
        .await;
    });

    (StatusCode::OK, Json(serde_json::json!({ "status": "started" })))
}

async fn run_repair(hub: sentryusb_ws::Hub, device: String, mode: RepairMode) {
    let mut log = RepairLog { hub: hub.clone(), buf: String::new() };
    let t = CMD_TIMEOUT;
    let (auto, force_allowed) = match mode {
        RepairMode::Interactive { confirm_destructive } => (false, confirm_destructive),
        RepairMode::AutoBoot { force_allowed } => (true, force_allowed),
    };

    log.line(
        "preflight",
        format!("Repairing {device} (XFS backingfiles); auto={auto} force_allowed={force_allowed}"),
    );

    // Quiesce archive and gadget, never this web server.
    log.line("quiesce", "Stopping the archive loop and USB gadget (the web UI stays up)…");
    let _ = run_capture(t, "systemctl", &["stop", "dashusb-archive"]).await;
    let _ = run_capture(t, "bash", &["-c", "killall archiveloop 2>/dev/null || true"]).await;
    match tokio::task::spawn_blocking(sentryusb_gadget::disable).await {
        Ok(Ok(())) => log.line("quiesce", "USB gadget disabled."),
        Ok(Err(e)) => log.line("quiesce", format!("Gadget disable warning (continuing): {e}")),
        Err(e) => log.line("quiesce", format!("Gadget disable task error (continuing): {e}")),
    }

    // Release mounts before repair.
    log.line("unmount", "Releasing mounts…");
    let mut mps: Vec<&str> = SUBMOUNTS.to_vec();
    mps.push(BACKINGFILES);
    for mp in mps {
        let r = run_capture(t, "umount", &[mp]).await;
        if !r.ok && !r.output.contains("not mounted") && !r.output.contains("not found") {
            log.cmd("unmount", &format!("umount {mp}"), &r);
        }
    }
    let r = run_capture(t, "umount", &[device.as_str()]).await;
    if !r.ok && !r.output.contains("not mounted") {
        log.cmd("unmount", &format!("umount {device}"), &r);
    }

    // Diagnose without changes.
    log.line("dryrun", "Running read-only check (xfs_repair -n)…");
    let dry = run_capture(t, "xfs_repair", &["-n", &device]).await;
    log.cmd("dryrun", &format!("xfs_repair -n {device}"), &dry);

    // Repair non-destructively, replaying the log by mount when needed.
    log.line("repair", "Attempting repair (xfs_repair)…");
    let mut rep = run_capture(t, "xfs_repair", &[&device]).await;
    log.cmd("repair", &format!("xfs_repair {device}"), &rep);

    if !rep.ok && needs_log_replay(&rep.output) {
        log.line("repair", "Dirty log detected — mounting to replay it, then retrying…");
        let m = run_capture(t, "mount", &[&device, BACKINGFILES]).await;
        log.cmd("repair", &format!("mount {device} {BACKINGFILES}"), &m);
        if m.ok {
            let u = run_capture(t, "umount", &[BACKINGFILES]).await;
            log.cmd("repair", &format!("umount {BACKINGFILES}"), &u);
            rep = run_capture(t, "xfs_repair", &[&device]).await;
            log.cmd("repair", &format!("xfs_repair {device} (after log replay)"), &rep);
        } else {
            log.line("repair", "Mount failed — the log cannot be replayed this way.");
        }
    }

    // Apply the mode-specific destructive escalation gate.
    let mut force_ran = false;
    match escalation_action(rep.ok, needs_log_replay(&rep.output), auto, force_allowed) {
        Escalation::Proceed | Escalation::Fail => {}
        Escalation::StopNeedsForce => {
            let log_file = persist_log(&log.buf);
            hub.broadcast(
                "storage_repair",
                &serde_json::json!({
                    "status": "needs_force",
                    "device": device,
                    "log_file": log_file,
                    "message": "The filesystem log is damaged and can't be replayed. The only repair left destroys the pending XFS log (xfs_repair -L), which may lose the most recently written metadata — typically a few of the newest clips. Confirm to proceed.",
                }),
            );
            if auto {
                notify_storage_repair(MSG_NEEDS_MANUAL_FORCE).await;
            }
            return;
        }
        Escalation::RunForce => {
            log.line(
                "repair",
                if auto {
                    "Auto force fix enabled — clearing the XFS log (xfs_repair -L)…"
                } else {
                    "Confirmed — clearing the XFS log (xfs_repair -L)…"
                },
            );
            rep = run_capture(t, "xfs_repair", &["-L", &device]).await;
            log.cmd("repair", &format!("xfs_repair -L {device}"), &rep);
            force_ran = true;
        }
    }

    if !rep.ok {
        let log_file = persist_log(&log.buf);
        hub.broadcast(
            "storage_repair",
            &serde_json::json!({
                "status": "error",
                "device": device,
                "log_file": log_file,
                "error": "xfs_repair could not repair the filesystem. Review the log — the drive itself may be failing (check the SSD's power, cable and enclosure).",
            }),
        );
        if auto {
            notify_storage_repair(MSG_HARD_FAIL).await;
        }
        return;
    }

    // Verify contents, then leave a clean reboot mount.
    log.line("verify", "Repair succeeded — verifying contents…");
    let mut cam_present = false;
    let mut lost_found = 0usize;
    let m = run_capture(t, "mount", &[&device, BACKINGFILES]).await;
    if m.ok {
        cam_present = cam_disk_present();
        if let Ok(rd) = std::fs::read_dir(format!("{BACKINGFILES}/lost+found")) {
            lost_found = rd.flatten().count();
        }
        log.line(
            "verify",
            format!("cam_disk.bin present: {cam_present}; lost+found entries: {lost_found}"),
        );
        let _ = run_capture(t, "umount", &[BACKINGFILES]).await;
    } else {
        log.cmd("verify", &format!("mount {device} {BACKINGFILES}"), &m);
        log.line("verify", "Could not mount after repair to verify — the reboot will retry the mount.");
    }

    // Interactive mode stops in a reboot-required state.
    let log_file = persist_log(&log.buf);
    let message = if cam_present {
        "Repair complete. A reboot is required to bring camera storage back online.".to_string()
    } else {
        "Repair complete, but cam_disk.bin is missing. After rebooting you'll need to recreate the backing files by re-running the Setup Wizard. A reboot is required first.".to_string()
    };
    hub.broadcast(
        "storage_repair",
        &serde_json::json!({
            "status": "reboot_required",
            "device": device,
            "cam_disk_present": cam_present,
            "lost_found_count": lost_found,
            "log_file": log_file,
            "message": message,
        }),
    );

    // Auto mode notifies, then reboots.
    if auto {
        let mut push = if force_ran { MSG_FORCE_SUCCESS } else { MSG_AUTO_SUCCESS }.to_string();
        if !cam_present {
            push.push_str(" Note: cam_disk.bin is missing — re-run the Setup Wizard to recreate the backing files after the reboot.");
        }
        notify_storage_repair(&push).await;
        tracing::info!("[storage-boot] auto repair complete — rebooting");
        // Notifications finish before the same reboot mechanism used by the API.
        let _ = run_capture(t, "reboot", &[]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_disk_strips_partition_suffix() {
        assert_eq!(parent_disk("/dev/sda2"), "sda");
        assert_eq!(parent_disk("sda2"), "sda");
        assert_eq!(parent_disk("/dev/sda"), "sda");
        assert_eq!(parent_disk("mmcblk0p2"), "mmcblk0");
        assert_eq!(parent_disk("/dev/mmcblk0p2"), "mmcblk0");
        assert_eq!(parent_disk("mmcblk0"), "mmcblk0");
        assert_eq!(parent_disk("nvme0n1p3"), "nvme0n1");
        assert_eq!(parent_disk("nvme0n1"), "nvme0n1");
    }

    #[test]
    fn resolve_mount_source_finds_backingfiles() {
        let mounts = "\
sysfs /sys sysfs rw 0 0
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
/dev/sda1 /mutable ext4 rw 0 0
/dev/sda2 /backingfiles xfs rw,noatime 0 0
tmpfs /run tmpfs rw 0 0";
        assert_eq!(resolve_mount_source(mounts, "/backingfiles").as_deref(), Some("/dev/sda2"));
        assert_eq!(resolve_mount_source(mounts, "/").as_deref(), Some("/dev/mmcblk0p2"));
        assert_eq!(resolve_mount_source(mounts, "/mutable").as_deref(), Some("/dev/sda1"));
        assert_eq!(resolve_mount_source(mounts, "/nope"), None);
    }

    #[test]
    fn resolve_mount_source_ignores_short_lines() {
        // A malformed/short line must not panic or mis-resolve.
        assert_eq!(resolve_mount_source("garbage\n/dev/sda2 /backingfiles xfs rw 0 0", "/backingfiles").as_deref(), Some("/dev/sda2"));
    }

    #[test]
    fn xfs_error_filter_ignores_benign_mount_lines() {
        let dev = "sda2";
        // Healthy mount lifecycle chatter.
        assert!(!is_xfs_error_line(
            "[Sun Jun 14 05:34:14 2026] XFS (sda2): Mounting V5 Filesystem b1a5fe90",
            dev
        ));
        assert!(!is_xfs_error_line(
            "[Sun Jun 14 05:34:14 2026] XFS (sda2): Ending clean mount",
            dev
        ));
        // Real errors from the actual incident MUST still be flagged.
        assert!(is_xfs_error_line("XFS (sda2): Metadata CRC error detected", dev));
        assert!(is_xfs_error_line(
            "XFS (sda2): Filesystem has been shut down due to log error (0x2).",
            dev
        ));
        assert!(is_xfs_error_line("XFS (sda2): log mount/recovery failed: error -74", dev));
        // A different volume's noise is ignored.
        assert!(!is_xfs_error_line("XFS (sdb1): Metadata CRC error detected", dev));
        // Non-XFS lines are ignored.
        assert!(!is_xfs_error_line("EXT4-fs (sda1): error count", dev));
    }

    #[test]
    fn decide_boot_action_covers_all_branches() {
        use BootAction::*;
        // Disabled auto-repair never touches storage.
        assert_eq!(decide_boot_action(false, true, true, false, false), Skip("storage_auto_repair disabled"));
        // Missing or incomplete storage is ineligible.
        assert_eq!(decide_boot_action(true, false, false, false, false), Skip("no backingfiles device found"));
        // Internal drives are ineligible.
        assert_eq!(decide_boot_action(true, true, false, false, false), Skip("backingfiles not on an external drive"));
        // Healthy mounts clear stale incident state.
        assert_eq!(decide_boot_action(true, true, true, true, false), ClearMarker);
        assert_eq!(decide_boot_action(true, true, true, true, true), ClearMarker);
        // Do not loop after a previous attempt.
        assert_eq!(decide_boot_action(true, true, true, false, true), NotifyRepeatCorruption);
        // First incident is eligible for repair.
        assert_eq!(decide_boot_action(true, true, true, false, false), Repair);
    }

    #[test]
    fn escalation_auto_forces_on_any_failure_interactive_only_on_dirty_log() {
        use Escalation::*;
        // Success proceeds in either mode.
        assert_eq!(escalation_action(true, false, true, true), Proceed);
        assert_eq!(escalation_action(true, true, false, false), Proceed);
        // Auto mode applies the user's broad force authorization.
        assert_eq!(escalation_action(false, false, true, true), RunForce);
        assert_eq!(escalation_action(false, true, true, true), RunForce);
        // Auto mode without force authorization stops.
        assert_eq!(escalation_action(false, false, true, false), StopNeedsForce);
        assert_eq!(escalation_action(false, true, true, false), StopNeedsForce);
        // Interactive force applies only to a confirmed dirty log.
        assert_eq!(escalation_action(false, true, false, true), RunForce);
        assert_eq!(escalation_action(false, true, false, false), StopNeedsForce);
        assert_eq!(escalation_action(false, false, false, true), Fail);
        assert_eq!(escalation_action(false, false, false, false), Fail);
    }

    #[test]
    fn marker_roundtrip() {
        let path = std::env::temp_dir()
            .join(format!("sentryusb_marker_test_{}", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        clear_marker(&path); // clean slate even after a failed prior run
        assert!(!marker_exists(&path));
        write_marker(&path);
        assert!(marker_exists(&path));
        clear_marker(&path);
        assert!(!marker_exists(&path));
        clear_marker(&path); // idempotent on missing file
    }

    #[test]
    fn needs_log_replay_matches_xfs_repair_phrases() {
        // The exact ERROR xfs_repair prints when the log must be replayed.
        let replay = "ERROR: The filesystem has valuable metadata changes in a log which needs to\nbe replayed. Mount the filesystem to replay the log, and unmount it before\nre-running xfs_repair. If you are unable to mount the filesystem, then use\nthe -L option to destroy the log and attempt a repair.";
        assert!(needs_log_replay(replay));
        // A clean repair run must not trip the gate.
        let clean = "Phase 1 - find and verify superblock...\nPhase 7 - verify and correct link counts...\ndone";
        assert!(!needs_log_replay(clean));
    }
}
