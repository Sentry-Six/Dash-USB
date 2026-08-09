//! Status, storage, config, and WiFi API handlers.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::router::AppState;

// Status cache
//
// The dashboard polls /api/status every 2 s per open tab. Uncached, each call
// forks 5-6 subprocesses (iwgetid, iwconfig, ip×2, ethtool, stat) for the
// WiFi/Ethernet/disk info: measurable CPU and fork+exec page faults on a Pi
// Zero 2 W, for data that barely changes.
//
// Only the slow parts are cached, with TTLs matching how often each value
// realistically changes:
//   * Network info (SSID, IP, signal, ethtool):  10 s
//   * Disk space (total/free via statvfs):        5 s
//
// CPU temp, fan speed, uptime and gadget state stay live; they are cheap /sys
// reads.

#[derive(Clone, Default)]
struct CachedNetwork {
    wifi_ssid: String,
    /// Channel frequency in Hz as a string ("5180000000" = 5.18 GHz), empty
    /// when not connected or `iw` isn't installed. The iOS client formats it
    /// via `formatFreqGHz`; the web UI ignores it. Cached with the rest of the
    /// wifi info because it only changes on reconnect or roam.
    wifi_freq: String,
    wifi_ip: String,
    ether_ip: String,
    ether_speed: String,
    /// Cached to avoid re-scanning /sys/class/net every poll. Signal strength
    /// and throughput are deliberately NOT cached: `get_status` reads
    /// `wifi_strength`/`wifi_signal_dbm` live from /proc/net/wireless (one
    /// file read, no shell-out) and derives the bps values from the
    /// net_sampler. Everything else here changes only on reconnect or cable
    /// swap.
    wifi_dev: String,
    eth_dev: String,
}

/// Live signal read from /proc/net/wireless, with no fork+exec.
///
/// Returns `(strength_as_X/70, signal_dbm)`. The `/70` denominator
/// matches what mainline mac80211 drivers (Broadcom Cypress on Pi 4/5
/// and Pi Zero 2 W, Realtek on most third-party chipsets) report as
/// the max link quality; other drivers may scale slightly differently,
/// in which case the WifiBars indicator is approximate but the dBm
/// value the UI also shows is always exact.
///
/// /proc/net/wireless format:
/// ```text
/// Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
///  face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
///  wlan0: 0000   58.  -52.  -256        0      0      0      0    137        0
/// ```
fn read_wireless_quality(dev: &str) -> Option<(String, Option<i32>)> {
    let data = std::fs::read_to_string("/proc/net/wireless").ok()?;
    for line in data.lines().skip(2) {
        let line = line.trim_start();
        // The kernel emits "wlan0:" with no space before the colon.
        let prefix = format!("{}:", dev);
        if !line.starts_with(&prefix) {
            continue;
        }
        let cols: Vec<&str> = line[prefix.len()..].split_whitespace().collect();
        // [status, link, level, noise, ...]
        if cols.len() < 3 {
            return None;
        }
        // Values end with a `.` (e.g. "58." for fixed-point); strip it.
        let link = cols[1].trim_end_matches('.').parse::<u32>().ok()?;
        let level = cols[2].trim_end_matches('.').parse::<i32>().ok();
        return Some((format!("{}/70", link), level));
    }
    None
}

#[derive(Clone, Copy, Default)]
struct CachedStorage {
    total_space: u64,
    free_space: u64,
}

struct StatusCache {
    network: Mutex<Option<(CachedNetwork, Instant)>>,
    storage: Mutex<Option<(CachedStorage, Instant)>>,
}

static STATUS_CACHE: OnceLock<StatusCache> = OnceLock::new();

fn cache() -> &'static StatusCache {
    STATUS_CACHE.get_or_init(|| StatusCache {
        network: Mutex::new(None),
        storage: Mutex::new(None),
    })
}

const NETWORK_TTL: Duration = Duration::from_secs(10);
const STORAGE_TTL: Duration = Duration::from_secs(5);

/// `(total_bytes, free_bytes)` for `/backingfiles`, or `None` on failure. One
/// statvfs syscall rather than a fork of `stat`; the path is `/backingfiles/.`
/// to match the `stat --file-system` target it replaced.
fn statvfs_backing_files() -> Option<(u64, u64)> {
    let path = std::ffi::CString::new("/backingfiles/.").ok()?;
    // SAFETY: zero-init is the documented init pattern for libc structs;
    // we check the return code before reading fields.
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statvfs(path.as_ptr(), &mut buf) };
    if r != 0 {
        return None;
    }
    let frsize = buf.f_frsize as u64;
    let total = (buf.f_blocks as u64).saturating_mul(frsize);
    let free = (buf.f_bfree as u64).saturating_mul(frsize);
    Some((total, free))
}

async fn cached_storage() -> CachedStorage {
    {
        let guard = cache().storage.lock().unwrap();
        if let Some((info, when)) = &*guard {
            if when.elapsed() < STORAGE_TTL {
                return *info;
            }
        }
    }
    let info = statvfs_backing_files()
        .map(|(t, f)| CachedStorage { total_space: t, free_space: f })
        .unwrap_or_default();
    let mut guard = cache().storage.lock().unwrap();
    *guard = Some((info, Instant::now()));
    info
}

// Network throughput sampler

#[derive(Clone)]
pub struct NetSample {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub taken_at: Instant,
}

pub type NetSampler = Arc<Mutex<HashMap<String, NetSample>>>;

// GET /api/status

#[derive(Serialize)]
struct PiStatus {
    cpu_temp: String,
    num_snapshots: String,
    snapshot_oldest: String,
    snapshot_newest: String,
    total_space: String,
    free_space: String,
    uptime: String,
    drives_active: String,
    /// Host-link state from /sys/class/udc/<udc>/state. "configured" means the
    /// car is enumerated and talking; "suspended" or "not attached" means it
    /// is not, even when drives_active says "yes". drives_active reflects only
    /// the configfs binding (the Pi's *intent* to present), which stays "yes"
    /// through a dead link.
    udc_state: String,
    /// Seconds since the car last wrote to cam_disk.bin (mtime age),
    /// -1 when unknown. Same signal the telemetry heartbeat uses.
    cam_last_write_secs: i64,
    wifi_ssid: String,
    /// Frequency in Hz as a string (e.g. "5180000000" for 5.18 GHz).
    /// Empty when not on WiFi or `iw` isn't installed. iOS renders this
    /// as "5.2 GHz" in the dashboard Wi-Fi sub-line via `formatFreqGHz`;
    /// the web UI doesn't currently use it.
    wifi_freq: String,
    wifi_strength: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wifi_signal_dbm: Option<i32>,
    wifi_ip: String,
    ether_ip: String,
    ether_speed: String,
    sbc_model: String,
    fan_speed: String,
    wifi_rx_bps: u64,
    wifi_tx_bps: u64,
    ether_rx_bps: u64,
    ether_tx_bps: u64,
    /// Stable per-device suffix derived from the system hostname (the
    /// part after the final `-`, e.g. "dashusb-A3F1" → "A3F1"). iOS
    /// uses this for the dashboard hero-bar identifier so devices paired
    /// over WiFi (no BLE metadata path) still show "Dash USB-A3F1"
    /// instead of bare "Dash USB". Empty if `/etc/hostname` is
    /// unreadable or has no dash.
    device_suffix: String,
}

pub async fn get_status(
    State(state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    // The sysfs/procfs reads are cheap, but the snapshot scan and
    // cam_last_write_secs hit /backingfiles — seconds while an archive run
    // saturates the disk. This is the endpoint the web UI's connection banner
    // polls, so blocking an async worker here surfaces to the user as
    // "Reconnecting". Run the whole synchronous FS half on the blocking pool.
    let mut s = match tokio::task::spawn_blocking(status_fs_snapshot).await {
        Ok(s) => s,
        Err(e) => {
            return crate::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("status task: {}", e),
            );
        }
    };

    let storage = cached_storage().await;
    if storage.total_space > 0 {
        s.total_space = storage.total_space.to_string();
        s.free_space = storage.free_space.to_string();
    }

    // IPs, SSID and ether_speed come from the NETWORK_TTL cache. Signal
    // strength, dBm (from /proc/net/wireless) and throughput (from the
    // net_sampler loop) are read live every poll: a 10 s lag on a
    // signal-strength indicator reads as broken.
    let net = cached_network().await;
    s.wifi_ssid = net.wifi_ssid;
    s.wifi_freq = net.wifi_freq;
    s.wifi_ip = net.wifi_ip;
    s.ether_ip = net.ether_ip;
    s.ether_speed = net.ether_speed;
    if !net.wifi_dev.is_empty() {
        if let Some((strength, dbm)) = read_wireless_quality(&net.wifi_dev) {
            s.wifi_strength = strength;
            s.wifi_signal_dbm = dbm;
        }
        let (rx, tx) = compute_throughput(&state.net_sampler, &net.wifi_dev);
        s.wifi_rx_bps = rx;
        s.wifi_tx_bps = tx;
    }
    if !net.eth_dev.is_empty() {
        let (rx, tx) = compute_throughput(&state.net_sampler, &net.eth_dev);
        s.ether_rx_bps = rx;
        s.ether_tx_bps = tx;
    }

    (StatusCode::OK, Json(serde_json::to_value(s).unwrap_or_default()))
}

/// The synchronous, filesystem-touching half of [`get_status`]. Split out so
/// it can run on the blocking pool.
fn status_fs_snapshot() -> PiStatus {
    let mut s = PiStatus {
        cpu_temp: String::new(),
        num_snapshots: "0".into(),
        snapshot_oldest: String::new(),
        snapshot_newest: String::new(),
        total_space: String::new(),
        free_space: String::new(),
        uptime: String::new(),
        drives_active: "no".into(),
        udc_state: String::new(),
        cam_last_write_secs: -1,
        wifi_ssid: String::new(),
        wifi_freq: String::new(),
        wifi_strength: String::new(),
        wifi_signal_dbm: None,
        wifi_ip: String::new(),
        ether_ip: String::new(),
        ether_speed: String::new(),
        sbc_model: String::new(),
        fan_speed: String::new(),
        wifi_rx_bps: 0,
        wifi_tx_bps: 0,
        ether_rx_bps: 0,
        ether_tx_bps: 0,
        device_suffix: read_device_suffix(),
    };

    s.sbc_model = get_sbc_model();

    if let Ok(data) = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp") {
        s.cpu_temp = data.trim().to_string();
    }

    s.fan_speed = read_fan_speed();

    if let Ok(data) = std::fs::read_to_string("/proc/uptime") {
        if let Some(secs) = data.split_whitespace().next() {
            s.uptime = secs.to_string();
        }
    }

    // Report active only when the UDC is bound AND lun.0 has a backing file.
    // A directory-exists check reports "yes" right through a partial teardown
    // where the car has already lost the device, leaving the dashboard green
    // after a failed toggle.
    if sentryusb_gadget::is_active() {
        s.drives_active = "yes".into();
    }
    s.udc_state = read_udc_state();
    s.cam_last_write_secs = cam_last_write_secs();

    let snapshots = scan_snapshots();
    s.num_snapshots = snapshots.count.to_string();
    if let Some(oldest) = snapshots.oldest {
        s.snapshot_oldest = oldest.to_string();
    }
    if let Some(newest) = snapshots.newest {
        s.snapshot_newest = newest.to_string();
    }

    s
}

/// Refresh-on-stale wrapper around the heavy WiFi + Ethernet shell-outs,
/// returning a cheap-to-clone snapshot. The lock is released across the
/// refresh, so concurrent callers that all see a stale entry each re-fetch.
async fn cached_network() -> CachedNetwork {
    {
        let guard = cache().network.lock().unwrap();
        if let Some((info, when)) = &*guard {
            if when.elapsed() < NETWORK_TTL {
                return info.clone();
            }
        }
    }
    let info = compute_network_info().await;
    let mut guard = cache().network.lock().unwrap();
    *guard = Some((info.clone(), Instant::now()));
    info
}

/// Uncached WiFi + Ethernet shell-outs. Reach it through [`cached_network`].
async fn compute_network_info() -> CachedNetwork {
    let mut info = CachedNetwork::default();

    // Skip the shell queries when the interface is down: they cost 5-10 s on
    // ethernet-only systems where wlan0 exists but is unconfigured. Only the
    // SSID, IP and frequency are needed here; signal strength and dBm are read
    // live from /proc/net/wireless on every status poll.
    let wifi_dev = find_net_device("wl*");
    if !wifi_dev.is_empty() && iface_is_up(&wifi_dev) {
        info.wifi_dev = wifi_dev.clone();
        let ssid_args = ["-r", wifi_dev.as_str()];
        let ip_args = ["-4", "addr", "show", wifi_dev.as_str()];
        // `iw dev <iface> link` prints "freq: 5180" (MHz) for one extra fork.
        let iw_args = ["dev", wifi_dev.as_str(), "link"];
        let (ssid_r, ip_r, iw_r) = tokio::join!(
            sentryusb_shell::run("iwgetid", &ssid_args),
            sentryusb_shell::run("ip", &ip_args),
            sentryusb_shell::run("iw", &iw_args),
        );
        if let Ok(out) = ssid_r {
            info.wifi_ssid = out.trim().to_string();
        }
        if let Ok(out) = ip_r {
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("inet ") {
                    if let Some(addr) = trimmed.split_whitespace().nth(1) {
                        info.wifi_ip = addr.split('/').next().unwrap_or("").to_string();
                    }
                }
            }
        }
        if let Ok(out) = iw_r {
            for line in out.lines() {
                let trimmed = line.trim();
                // Format: `freq: 5180` (MHz). Convert to Hz string so iOS
                // `Formatters.formatFreqGHz` can divide by 1e9 and render
                // "5.2 GHz" without further parsing.
                if let Some(rest) = trimmed.strip_prefix("freq:") {
                    if let Ok(mhz) = rest.trim().parse::<u64>() {
                        info.wifi_freq = (mhz * 1_000_000).to_string();
                        break;
                    }
                }
            }
        }
    }

    // Ethernet: same operstate guard.
    let mut eth_dev = find_net_device("eth*");
    if eth_dev.is_empty() {
        eth_dev = find_net_device("en*");
    }
    if !eth_dev.is_empty() && iface_is_up(&eth_dev) {
        info.eth_dev = eth_dev.clone();
        let eth_ip_args = ["-4", "addr", "show", eth_dev.as_str()];
        let eth_tool_args = [eth_dev.as_str()];
        let (ip_r, ethtool_r) = tokio::join!(
            sentryusb_shell::run("ip", &eth_ip_args),
            sentryusb_shell::run("ethtool", &eth_tool_args),
        );
        if let Ok(out) = ip_r {
            for line in out.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("inet ") {
                    if let Some(addr) = trimmed.split_whitespace().nth(1) {
                        info.ether_ip = addr.split('/').next().unwrap_or("").to_string();
                    }
                }
            }
        }
        if let Ok(out) = ethtool_r {
            for line in out.lines() {
                if line.contains("Speed:") {
                    if let Some(val) = line.split(':').nth(1) {
                        info.ether_speed = val.trim().to_string();
                    }
                }
            }
        }
    }

    info
}

// GET /api/status/storage

#[derive(Serialize)]
struct StorageBreakdown {
    cam_size: i64,
    snapshots_size: i64,
    total_space: i64,
    free_space: i64,
}

pub async fn get_storage_breakdown(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let cam = disk_usage("/backingfiles/cam_disk.bin").await;
    let mut sb = StorageBreakdown {
        cam_size: cam,
        snapshots_size: 0,
        total_space: 0,
        free_space: 0,
    };

    // Polled at 10 s, so read fresh here rather than cache.
    if let Some((total, free)) = statvfs_backing_files() {
        sb.total_space = total as i64;
        sb.free_space = free as i64;
    }

    // Derive snapshot usage by subtraction (reflink clones make du unreliable)
    let disk_images = sb.cam_size;
    let used = sb.total_space - sb.free_space;
    sb.snapshots_size = (used - disk_images).max(0);

    (StatusCode::OK, Json(serde_json::to_value(sb).unwrap_or_default()))
}

// GET /api/config

pub async fn get_config(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let has = |p: &str| -> String {
        if std::path::Path::new(p).exists() { "yes".into() } else { "no".into() }
    };

    (StatusCode::OK, Json(serde_json::json!({
        "has_cam": has("/backingfiles/cam_disk.bin"),
    })))
}

// GET /api/wifi

pub async fn get_wifi_config(
    State(_state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut ssid = String::new();
    let mut connected = false;
    let mut source = String::new();

    // 1. iwgetid first: it reads the current association out of the kernel
    //    and returns instantly. `nmcli dev wifi` below triggers a full AP
    //    SCAN, which takes seconds and briefly disrupts the link — a steep
    //    price when the answer is almost always "the SSID we are already on".
    if let Ok(out) = sentryusb_shell::run("iwgetid", &["-r"]).await {
        let s = out.trim();
        if !s.is_empty() {
            ssid = s.to_string();
            connected = true;
            source = "iwgetid".into();
        }
    }

    // 2. Fallback: nmcli (scanning)
    if ssid.is_empty() {
        if let Ok(out) =
            sentryusb_shell::run("nmcli", &["-t", "-f", "active,ssid", "dev", "wifi"]).await
        {
            for line in out.lines() {
                if line.starts_with("yes:") {
                    ssid = line.strip_prefix("yes:").unwrap_or("").to_string();
                    connected = true;
                    source = "networkmanager".into();
                    break;
                }
            }
        }
    }

    // 3. Fallback: wpa_supplicant.conf
    if ssid.is_empty() {
        for p in &[
            "/etc/wpa_supplicant/wpa_supplicant.conf",
            "/boot/firmware/wpa_supplicant.conf",
            "/boot/wpa_supplicant.conf",
        ] {
            if let Ok(data) = std::fs::read_to_string(p) {
                for line in data.lines() {
                    let trimmed = line.trim();
                    if let Some(val) = trimmed.strip_prefix("ssid=") {
                        let val = val.trim_matches('"');
                        if !val.is_empty() {
                            ssid = val.to_string();
                            source = "wpa_supplicant".into();
                            break;
                        }
                    }
                }
                if !ssid.is_empty() {
                    break;
                }
            }
        }
    }

    // 4. Config SSID
    let mut config_ssid = String::new();
    let config_path = sentryusb_config::find_config_path();
    if let Ok((active, _)) = sentryusb_config::parse_file(config_path) {
        if let Some(v) = active.get("SSID") {
            config_ssid = v.clone();
        }
    }
    // Filter placeholder values
    let lower = config_ssid.to_lowercase();
    if matches!(lower.as_str(), "your_ssid" | "yourssid" | "your_wifi" | "ssid" | "your_network" | "") {
        config_ssid.clear();
    }

    let mut wlan_country = String::new();
    if let Ok(out) = sentryusb_shell::run("iw", &["reg", "get"]).await {
        for line in out.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("country") {
                let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
                if parts.len() >= 2 {
                    wlan_country = parts[1].trim_end_matches(':').to_string();
                }
                break;
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "current": {
            "ssid": ssid,
            "connected": connected,
            "source": source,
        },
        "config_ssid": config_ssid,
        "wlan_country": wlan_country,
    })))
}

// Helpers

/// List snapshot backing files at the top of `/backingfiles/snapshots/`.
///
/// How many snapshots exist, and the oldest/newest `snap.bin` mtime.
struct SnapshotScan {
    count: usize,
    oldest: Option<u64>,
    newest: Option<u64>,
}

/// Scan exactly one directory level: no recursion, no symlink follow.
/// Snapshots always keep `snap.bin` at the top level
/// (`/backingfiles/snapshots/snap-NNNNNN/snap.bin`), and a recursive walk
/// descends through every snapshot's `mnt -> /tmp/snapshots/snap-NNN` symlink,
/// which is an autofs mount (timeout=300s) that re-mounts the per-snapshot
/// vfat loop device on first access. Each /api/status call after the autofs
/// timeout then triggered up to 130 vfat mounts *and* walked the entire dashcam
/// tree inside each one: 15,000+ openat syscalls per request, 5-15s TTFB.
///
/// Folds the mtime extremes rather than returning a sorted path list. The old
/// version sorted LEXICALLY and read the first and last entries as
/// oldest/newest, but slot numbers are not time-monotonic in the field — a
/// reflash can leave a stale high-numbered snapshot sitting above a restarted
/// sequence — so whenever the highest-numbered snapshot was the older one, the
/// dashboard rendered its date range backwards.
fn scan_snapshots() -> SnapshotScan {
    let mut scan = SnapshotScan { count: 0, oldest: None, newest: None };
    let base = std::path::Path::new("/backingfiles/snapshots/");
    let Ok(entries) = std::fs::read_dir(base) else {
        return scan;
    };
    for entry in entries.flatten() {
        // Only consider entries that are themselves directories on the
        // host filesystem. `file_type()` uses the dirent's d_type and
        // does NOT follow symlinks, so the `mnt` autofs symlink inside
        // each snapshot is never resolved here.
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let snap_bin = entry.path().join("snap.bin");
        // Use symlink_metadata to avoid traversing into anything weird;
        // snap.bin is always a regular file on the parent XFS. Reusing it for
        // the mtime keeps this at the same syscall count as the old
        // existence check.
        let Ok(meta) = std::fs::symlink_metadata(&snap_bin) else {
            continue;
        };
        scan.count += 1;
        // A snapshot whose mtime is unreadable still counts; it just can't
        // move the range.
        let Ok(mtime) = meta.modified() else { continue };
        let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let secs = d.as_secs();
        scan.oldest = Some(scan.oldest.map_or(secs, |o: u64| o.min(secs)));
        scan.newest = Some(scan.newest.map_or(secs, |n: u64| n.max(secs)));
    }
    scan
}

/// Raspberry Pi cooling-fan RPM from its hwmon device; empty when absent.
fn read_fan_speed() -> String {
    let base = std::path::Path::new("/sys/devices/platform/cooling_fan/hwmon");
    let Ok(entries) = std::fs::read_dir(base) else {
        return String::new();
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("fan1_input");
        if let Ok(data) = std::fs::read_to_string(&candidate) {
            return data.trim().to_string();
        }
    }
    String::new()
}

/// Host-link state of the first UDC ("configured", "suspended",
/// "not attached", ...). Empty when there is no UDC (non-gadget dev box).
/// Shared with the health check so the pill and the health warning can
/// never disagree about what the link state means.
pub(crate) fn read_udc_state() -> String {
    let Ok(entries) = std::fs::read_dir("/sys/class/udc") else {
        return String::new();
    };
    for entry in entries.flatten() {
        if let Ok(data) = std::fs::read_to_string(entry.path().join("state")) {
            return data.trim().to_string();
        }
    }
    String::new()
}

/// Seconds since the last write to cam_disk.bin, -1 when unknown.
fn cam_last_write_secs() -> i64 {
    let Ok(meta) = std::fs::metadata("/backingfiles/cam_disk.bin") else {
        return -1;
    };
    meta.modified()
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(-1)
}

/// Last segment after the final `-` of the system hostname: "dashusb-A3F1" →
/// "A3F1". iOS renders this as the device-specific dashboard identifier even
/// when paired over WiFi; the BLE path has its own device-info channel, but
/// WiFi-only pairing can only learn the suffix from /status. Empty when the
/// hostname has no dash or `/etc/hostname` can't be read. The 8-char cap
/// guards against hostnames whose post-dash segment is a fully-qualified
/// domain piece rather than a stable identifier.
fn read_device_suffix() -> String {
    let hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if let Some(idx) = hostname.rfind('-') {
        let suffix = &hostname[idx + 1..];
        if !suffix.is_empty() && suffix.len() <= 8 {
            return suffix.to_string();
        }
    }
    String::new()
}

fn read_net_bytes(dev: &str, stat: &str) -> Option<u64> {
    let path = format!("/sys/class/net/{}/statistics/{}", dev, stat);
    std::fs::read_to_string(&path).ok()?.trim().parse::<u64>().ok()
}

fn compute_throughput(sampler: &NetSampler, dev: &str) -> (u64, u64) {
    let Some(rx_now) = read_net_bytes(dev, "rx_bytes") else { return (0, 0); };
    let Some(tx_now) = read_net_bytes(dev, "tx_bytes") else { return (0, 0); };
    let now = Instant::now();
    let mut map = sampler.lock().unwrap_or_else(|e| e.into_inner());
    let result = if let Some(prev) = map.get(dev) {
        let elapsed = now.duration_since(prev.taken_at).as_secs_f64();
        if elapsed < 0.1 {
            (0, 0)
        } else {
            let rx_bps = ((rx_now.saturating_sub(prev.rx_bytes) as f64 * 8.0) / elapsed) as u64;
            let tx_bps = ((tx_now.saturating_sub(prev.tx_bytes) as f64 * 8.0) / elapsed) as u64;
            (rx_bps, tx_bps)
        }
    } else {
        (0, 0)
    };
    map.insert(dev.to_string(), NetSample { rx_bytes: rx_now, tx_bytes: tx_now, taken_at: now });
    result
}

fn find_net_device(pattern: &str) -> String {
    let prefix = pattern.trim_end_matches('*');
    if let Ok(entries) = std::fs::read_dir("/sys/class/net/") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(prefix) {
                    return name.to_string();
                }
            }
        }
    }
    String::new()
}

/// True when the kernel reports the interface in `operstate == "up"`.
///
/// Gates the shell queries above: `iwgetid`/`iwconfig`/`ip` can each block for
/// several seconds on a present-but-DOWN interface (`wlan0` exists but no
/// NetworkManager, or Skip-WiFi configured), adding up to 5-15s to
/// `GET /api/status`. Companion apps probing this endpoint with a short HTTP
/// timeout then fall back to BLE-only mode even though the Pi is reachable
/// over ethernet.
fn iface_is_up(dev: &str) -> bool {
    let path = format!("/sys/class/net/{}/operstate", dev);
    std::fs::read_to_string(&path)
        .map(|s| s.trim() == "up")
        .unwrap_or(false)
}

async fn disk_usage(path: &str) -> i64 {
    // st_blocks * 512 is the true disk usage, correct for sparse files and
    // reflink copies. Async so the tokio worker isn't blocked: the companion
    // app polls this roughly every 15 s per device.
    if let Ok(out) = tokio::process::Command::new("stat")
        .args(["--format=%b", path])
        .output()
        .await
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Ok(blocks) = s.trim().parse::<i64>() {
                return blocks * 512;
            }
        }
    }
    0
}

/// SBC model string from the device tree, "unknown" when unreadable.
pub fn get_sbc_model() -> String {
    for p in &["/proc/device-tree/model", "/sys/firmware/devicetree/base/model"] {
        if let Ok(data) = std::fs::read(p) {
            return String::from_utf8_lossy(&data)
                .trim_end_matches('\0')
                .trim()
                .to_string();
        }
    }
    "unknown".to_string()
}
