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

/// Defer shutdown until the HTTP response flushes; try commands available
/// across both full and minimal images.
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
    // User toggles own the cycle lock so they cannot race archive or watchdog
    // cycles. Shim handlers stay lockless because archiveloop already holds it.
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

/// Idempotent shim handler. Do not acquire the gadget-cycle flock: archiveloop
/// holds it while calling back, so nested acquisition deadlocks.
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

/// Force a sync across archiveloop's reachable, unreachable, and Travel Mode
/// wait states. The unreachable canary must precede the reachable canary so an
/// idle completed cycle transitions before receiving the forced-positive.
pub async fn trigger_sync(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    tokio::spawn(async {
        let unreachable = std::path::Path::new("/tmp/archive_is_unreachable");
        let reachable = std::path::Path::new("/tmp/archive_is_reachable");
        // Transition a loop waiting for unreachability.
        let _ = std::fs::File::create(unreachable);
        // Remove an unconsumed canary to prevent a later phantom sync.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !unreachable.exists() {
                break;
            }
        }
        let _ = std::fs::remove_file(unreachable);
        // Force the reachable or Travel Mode wait to start a cycle.
        let _ = std::fs::File::create(reachable);
    });
    crate::json_ok()
}

/// Remount the root filesystem read-write. Writes under `/root` fail while the
/// image keeps `/` read-only.
fn remount_root_rw() {
    if let Err(e) = std::process::Command::new("bash")
        .args(["-c", "/root/bin/remountfs_rw"])
        .status()
    {
        tracing::warn!("remountfs_rw failed to run: {e}");
    }
}

/// Clear phone GATT bonds and app PIN, then restart only the phone-facing BLE
/// service. Never restart bluetooth.service, which would interrupt archiving.
pub async fn ble_reset_pair(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    // Preserve keyless cache entries while removing phone bonds.
    let removed = remove_phone_bonds().await;

    // Clear the PIN from the read-only-root installation and boot copy.
    remount_root_rw();
    let mut pin_cleared = false;
    for p in ["/root/.dashusb/ble-pin", "/boot/firmware/BLE_PIN"] {
        match std::fs::remove_file(p) {
            Ok(()) => pin_cleared = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("ble-reset: failed to remove {p}: {e}"),
        }
    }

    // Restart only the phone-facing GATT server.
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

/// Remove BlueZ peers carrying pairing keys, leaving keyless cache entries.
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
            if !has_bond_key(&info) {
                continue; // keyless cache entries are not stale-LTK
            }
            let _ = sentryusb_shell::run_with_timeout(
                Duration::from_secs(10),
                "bluetoothctl",
                &["remove", &mac],
            )
            .await;
            // Remove an on-disk bond unknown to the live daemon.
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
    // RTC hardware presence is stable; cache repeated dashboard reads.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=300")],
        Json(serde_json::json!({
            "available": rtc_exists,
            "time": rtc_time,
        })),
    )
}

/// Report clock trust for date-based pruning and deduplication. Warn only when
/// neither a recent/NTP-synced clock nor RTC fallback exists.
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

    // A short cache preserves NTP transition visibility.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=10")],
        Json(serde_json::json!({
            "synced": synced,
            "has_rtc": has_rtc,
            "ntp_synced": ntp_synced,
            // RTC-backed devices do not need the unsynced-clock warning.
            "show_warning": !synced && !has_rtc,
        })),
    )
}

pub async fn get_ssh_pubkey(State(_s): State<AppState>) -> impl IntoResponse {
    let pub_key = std::fs::read_to_string("/root/.ssh/id_ed25519.pub")
        .or_else(|_| std::fs::read_to_string("/root/.ssh/id_rsa.pub"))
        .unwrap_or_default();
    // Key generation is the only mutation; explicit reloads bypass this cache.
    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "private, max-age=3600")],
        Json(serde_json::json!({"public_key": pub_key.trim()})),
    )
}

pub async fn generate_ssh_key(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    // Remount production root; development images may lack the helper.
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
