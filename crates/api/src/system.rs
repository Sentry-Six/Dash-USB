//! System actions: reboot, toggle drives, BLE pair, speedtest, SSH, diagnostics, RTC.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use crate::router::AppState;

pub async fn reboot(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    tokio::spawn(async { let _ = sentryusb_shell::run("reboot", &[]).await; });
    crate::json_ok()
}

/// Spawned so the HTTP response can flush before the kernel starts tearing
/// things down. Falls back through `poweroff`, `shutdown -h now`, then
/// `systemctl poweroff`, since some minimal images ship only one of the three.
pub async fn shutdown(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    tokio::spawn(async {
        if sentryusb_shell::run("poweroff", &[]).await.is_ok() {
            return;
        }
        if sentryusb_shell::run("shutdown", &["-h", "now"]).await.is_ok() {
            return;
        }
        let _ = sentryusb_shell::run("systemctl", &["poweroff"]).await;
    });
    crate::json_ok()
}

pub async fn toggle_drives(State(_s): State<AppState>, _body: String) -> (StatusCode, Json<serde_json::Value>) {
    // A user toggle owns a full gadget cycle, so it MUST take the cross-process
    // flock archiveloop wraps around its own cycles. Without it a toggle races
    // an archive sync or a stall-watchdog recovery and flips the gadget while
    // cam_disk.bin is mounted on the host. gadget_enable/gadget_disable below
    // stay lockless: archiveloop's shims call them while it already holds this
    // flock.
    let result = tokio::task::spawn_blocking(|| -> Result<(), String> {
        let _cycle = sentryusb_gadget::cycle_lock::acquire(Duration::from_secs(30))
            .map_err(|e| format!("USB drives are busy ({}) — try again shortly", e))?;
        // Re-read under the lock: whoever held it may have flipped the state.
        let was_active = sentryusb_gadget::is_active();
        let r = if was_active {
            sentryusb_gadget::disable()
        } else {
            sentryusb_gadget::enable()
        };
        r.map_err(|e| {
            format!("USB gadget {} failed: {}", if was_active { "disable" } else { "enable" }, e)
        })
    })
    .await;
    match result {
        Ok(Ok(())) => crate::json_ok(),
        Ok(Err(msg)) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("USB gadget task panicked: {}", e),
        ),
    }
}

/// Idempotent set-to-active, called from the `/root/bin/enable_gadget.sh` shim
/// so archiveloop coordinates with this server instead of driving configfs in
/// parallel.
///
/// This handler and `gadget_disable` MUST NOT take the gadget-cycle flock.
/// archiveloop already holds it when the shim curls back in, so locking here
/// deadlocks: the shim's curl hangs until its --max-time kills the request.
pub async fn gadget_enable(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    if sentryusb_gadget::is_active() {
        return crate::json_ok();
    }
    match tokio::task::spawn_blocking(sentryusb_gadget::enable).await {
        Ok(Ok(())) => crate::json_ok(),
        Ok(Err(e)) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("USB gadget enable failed: {}", e),
        ),
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("USB gadget task panicked: {}", e),
        ),
    }
}

/// Idempotent set-to-inactive. Subject to the same no-flock rule as
/// [`gadget_enable`].
pub async fn gadget_disable(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    if !sentryusb_gadget::is_active() {
        return crate::json_ok();
    }
    match tokio::task::spawn_blocking(sentryusb_gadget::disable).await {
        Ok(Ok(())) => crate::json_ok(),
        Ok(Err(e)) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("USB gadget disable failed: {}", e),
        ),
        Err(e) => crate::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("USB gadget task panicked: {}", e),
        ),
    }
}

/// Force archiveloop to start a sync cycle now, whatever the connectivity
/// check currently thinks. Two wait states are possible when the user clicks
/// "Start Archive":
///
///   1. `wait_for_archive_to_be_reachable`, the usual case after a fresh boot
///      or after the car drove away from home WiFi. The loop polls
///      archive-is-reachable.sh and consumes `/tmp/archive_is_reachable` as a
///      forced positive.
///   2. `wait_for_archive_to_be_unreachable`, the idle state once an archive
///      finished. The loop consumes `/tmp/archive_is_unreachable` as a forced
///      "drove away" and returns to state 1.
///
/// Create the unreachable canary first, give archiveloop a moment to consume
/// it, then create the reachable canary. That order covers state 1 directly
/// and state 2 through the transition. Creating only the unreachable canary is
/// a no-op in state 1, exactly the case a user hits when the NAS is briefly
/// down or the reachability check is misconfigured.
///
/// Travel Mode has a third idle state, the paced sleep between cycles
/// (travel_mode_pace). It consumes the reachable canary too, cutting the sleep
/// short so "Start Archive" works on the road.
pub async fn trigger_sync(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    tokio::spawn(async {
        let unreachable = std::path::Path::new("/tmp/archive_is_unreachable");
        let reachable = std::path::Path::new("/tmp/archive_is_reachable");
        // Step 1: kick a loop sitting in wait_for_unreachable.
        let _ = std::fs::File::create(unreachable);
        // Wait up to ~5s for archiveloop to consume it. If it doesn't, the
        // loop is already in wait_for_reachable, and a stale canary would fire
        // on the next idle cycle as a phantom force-sync. Clean up either way.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !unreachable.exists() {
                break;
            }
        }
        let _ = std::fs::remove_file(unreachable);
        // Step 2: kick a loop sitting in wait_for_reachable. archiveloop
        // consumes this and starts a cycle even while the real network probe
        // is failing, which is what "Start Archive Now" means to a user.
        let _ = std::fs::File::create(reachable);
    });
    crate::json_ok()
}

/// Remount the root filesystem read-write. These images keep `/` read-only to
/// protect the SD card, so a write to `/root` silently no-ops until this runs.
fn remount_root_rw() {
    if let Err(e) = std::process::Command::new("bash")
        .args(["-c", "/root/bin/remountfs_rw"])
        .status()
    {
        tracing::warn!("remountfs_rw failed to run: {e}");
    }
}

/// Recovery for a wedged phone-to-Pi BLE pairing: the "Pairing rejected by
/// DashUSB-XXXX" dead end (#324), where the phone has no Bluetooth-settings
/// entry to forget. Clears ONLY phone-side state so a fresh claim can succeed:
///   - removes each phone GATT-client bond from BlueZ (`bluetoothctl remove`)
///   - deletes the app PIN (`/root/.dashusb/ble-pin` plus the boot copy),
///     returning the device to unclaimed
///   - restarts ONLY `dashusb-ble.service`, the phone-facing GATT server
///
/// MUST NOT restart `bluetooth.service`, so archiving keeps running. The app
/// pushes a fresh PIN on the re-claim.
///
/// The non-phone peer filter (see `is_tesla_peer`) is inherited from the
/// upstream Tesla product. GM exposes no BLE interface, so no such peer
/// exists here and the filter never matches. Harmless, but dead.
pub async fn ble_reset_pair(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    // 1) Remove phone bonds, preserving keyless entries.
    let removed = remove_phone_bonds().await;

    // 2) Clear the app PIN so the device returns to the unclaimed state and
    //    accepts a fresh claim from the app. Root is ro at runtime.
    remount_root_rw();
    let mut pin_cleared = false;
    for p in ["/root/.dashusb/ble-pin", "/boot/firmware/BLE_PIN"] {
        match std::fs::remove_file(p) {
            Ok(()) => pin_cleared = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("ble-reset: failed to remove {p}: {e}"),
        }
    }

    // 3) Restart ONLY the phone-facing GATT server, never
    //    bluetooth.service.
    let restarted = sentryusb_shell::run_with_timeout(
        Duration::from_secs(20),
        "systemctl",
        &["restart", "dashusb-ble.service"],
    )
    .await
    .is_ok();

    (StatusCode::OK, Json(serde_json::json!({
        "status": "reset",
        "removed_bonds": removed,
        "pin_cleared": pin_cleared,
        "ble_service_restarted": restarted,
    })))
}

/// Remove every BlueZ phone-client bond, leaving keyless cache entries alone.
///
/// Phone bonds carry an LTK/LinkKey that goes stale after a Pi rebuild or a
/// phone reset, the desync behind #324. Skip any peer that carries no bond key,
/// since those are not the stale-LTK problem. `bluetoothctl remove` drops the
/// bond from the live daemon and deletes the on-disk dir without restarting
/// bluetoothd.
///
/// The `is_tesla_peer` name-shape filter is inherited from the upstream Tesla
/// product and cannot match on a GM install. Kept because it is free and a Pi
/// reused from a Sentry USB build could still hold such an entry.
async fn remove_phone_bonds() -> Vec<String> {
    let mut removed = Vec::new();
    let adapters = match std::fs::read_dir("/var/lib/bluetooth") {
        Ok(d) => d,
        Err(_) => return removed,
    };
    for adapter in adapters.flatten() {
        let apath = adapter.path();
        if !apath.is_dir() {
            continue;
        }
        let peers = match std::fs::read_dir(&apath) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for peer in peers.flatten() {
            let ppath = peer.path();
            let mac = peer.file_name().to_string_lossy().to_string();
            if !is_mac_dir(&mac) {
                continue;
            }
            let info = std::fs::read_to_string(ppath.join("info")).unwrap_or_default();
            if is_tesla_peer(&info) || !has_bond_key(&info) {
                continue; // keyless cache entries are not stale-LTK
            }
            let _ = sentryusb_shell::run_with_timeout(
                Duration::from_secs(10),
                "bluetoothctl",
                &["remove", &mac],
            )
            .await;
            // If the daemon didn't know the bond, the dir survives `remove`,
            // so delete it directly and stop a stale LTK from lingering.
            let _ = std::fs::remove_dir_all(&ppath);
            removed.push(mac);
        }
    }
    removed
}

/// True for a BlueZ peer directory name, `XX:XX:XX:XX:XX:XX`.
fn is_mac_dir(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The peer's advertised `Name=` from its BlueZ `info` file, if present.
fn info_name(info: &str) -> Option<&str> {
    info.lines().find_map(|l| l.strip_prefix("Name=").map(str::trim))
}

/// True when the peer advertises a Tesla-shaped name, `S<hex>C` (for example
/// `Se04d38788e92e221C`). No GM vehicle advertises this; see
/// `remove_phone_bonds` for why the check survives.
fn is_tesla_peer(info: &str) -> bool {
    match info_name(info) {
        Some(name) => {
            let b = name.as_bytes();
            b.len() >= 10
                && b[0] == b'S'
                && b[b.len() - 1] == b'C'
                && b[1..b.len() - 1].iter().all(u8::is_ascii_hexdigit)
        }
        None => false,
    }
}

/// True when the peer's `info` carries an actual pairing key. Keyless cache
/// entries aren't the stale-LTK problem and are left alone.
fn has_bond_key(info: &str) -> bool {
    info.contains("[LinkKey]")
        || info.contains("[LongTermKey]")
        || info.contains("[PeripheralLongTermKey]")
        || info.contains("[SlaveLongTermKey]")
}

static SPEEDTEST_CHUNK: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

fn speedtest_chunk() -> &'static Vec<u8> {
    SPEEDTEST_CHUNK.get_or_init(|| {
        let mut buf = vec![0u8; 65536];
        for chunk in buf.chunks_mut(8) {
            let val = rand::random::<u64>();
            let bytes = val.to_le_bytes();
            let len = chunk.len().min(8);
            chunk[..len].copy_from_slice(&bytes[..len]);
        }
        buf
    })
}

pub async fn speedtest(State(_s): State<AppState>) -> impl IntoResponse {
    use axum::body::Body;

    let chunk = speedtest_chunk();
    let stream = tokio_stream::iter(
        (0..1000).map(move |_| Ok::<_, std::convert::Infallible>(chunk.clone()))
    );

    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
}

pub async fn get_rtc_status(State(_s): State<AppState>) -> impl IntoResponse {
    let rtc_exists = std::path::Path::new("/dev/rtc0").exists();
    let mut rtc_time = String::new();
    if rtc_exists {
        if let Ok(out) = sentryusb_shell::run("hwclock", &["-r"]).await {
            rtc_time = out.trim().to_string();
        }
    }
    // RTC presence is a hardware fact that can't change at runtime, and the
    // Dashboard hits this on every load. Let the browser serve the next 5 min
    // from cache.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=300")],
        Json(serde_json::json!({
            "available": rtc_exists,
            "time": rtc_time,
        })),
    )
}

/// Whether the Pi's system clock can be trusted. Snapshot pruning and archive
/// dedup both key off dated folder names, so a bogus clock buckets recordings
/// under the wrong date. The UI shows a short "clock not synced" hint only when
/// both hold:
///   * The system clock looks bogus (year < 2025, so unset or a Jan-1-2000
///     style fallback, and no NTP sync yet)
///   * No RTC battery is installed, so the clock can't survive reboots
///
/// NTP fixes it as soon as WiFi is up. The warning is informational, not
/// blocking.
///
/// Response shape:
/// ```json
/// {
///   "synced": true,            // year >= 2025 OR systemd-timesyncd marker
///   "has_rtc": true,           // /dev/rtc0 exists
///   "ntp_synced": true,        // /run/systemd/timesync/synchronized exists
///   "show_warning": false      // !synced && !has_rtc
/// }
/// ```
pub async fn get_clock_status(
    State(_s): State<AppState>,
) -> impl IntoResponse {
    let ntp_synced =
        std::path::Path::new("/run/systemd/timesync/synchronized").exists();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 2025-01-01 00:00:00 UTC = 1735689600.
    let year_looks_recent = secs > 1_735_689_600;
    let synced = ntp_synced || year_looks_recent;
    let has_rtc = std::path::Path::new("/dev/rtc0").exists();

    // NTP sync state flips at most a handful of times per boot, so a 10s cache
    // cuts repeat polling without hiding changes the BLE warning UI needs.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=10")],
        Json(serde_json::json!({
            "synced": synced,
            "has_rtc": has_rtc,
            "ntp_synced": ntp_synced,
            // The only boolean the UI acts on. Warn solely when the clock is
            // bad AND no hardware fallback exists, so RTC users see nothing.
            "show_warning": !synced && !has_rtc,
        })),
    )
}

pub async fn get_ssh_pubkey(State(_s): State<AppState>) -> impl IntoResponse {
    let pub_key = std::fs::read_to_string("/root/.ssh/id_ed25519.pub")
        .or_else(|_| std::fs::read_to_string("/root/.ssh/id_rsa.pub"))
        .unwrap_or_default();
    // The pubkey only changes when generate_ssh_key runs, so cache an hour and
    // let users reload explicitly after regenerating.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=3600")],
        Json(serde_json::json!({"public_key": pub_key.trim()})),
    )
}

pub async fn generate_ssh_key(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    // Production images run a read-only root, so writing to /root/.ssh fails
    // with EROFS unless remounted first. remountfs_rw is the canonical helper;
    // the mount fallback covers dev images that lack it.
    let _ = sentryusb_shell::run(
        "bash",
        &["-c", "/root/bin/remountfs_rw 2>/dev/null || mount -o remount,rw / 2>/dev/null || true"],
    )
    .await;

    let key_path = "/root/.ssh/id_ed25519";
    let _ = std::fs::remove_file(key_path);
    let _ = std::fs::remove_file(format!("{}.pub", key_path));
    let _ = std::fs::create_dir_all("/root/.ssh");

    match sentryusb_shell::run_with_timeout(
        Duration::from_secs(15),
        "ssh-keygen",
        &["-t", "ed25519", "-f", key_path, "-N", "", "-C", "dashusb"],
    ).await {
        Ok(_) => {
            let pub_key = std::fs::read_to_string(format!("{}.pub", key_path)).unwrap_or_default();
            (StatusCode::OK, Json(serde_json::json!({"public_key": pub_key.trim()})))
        }
        Err(e) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to generate SSH key: {}", e)),
    }
}
