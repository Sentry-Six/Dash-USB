//! System health check and diagnostics.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::router::AppState;

#[derive(Serialize)]
struct HealthItem {
    name: String,
    /// "pass" | "warn" | "fail"
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct HealthCategory {
    name: String,
    items: Vec<HealthItem>,
}

#[derive(Serialize)]
struct HealthReport {
    summary: String,
    categories: Vec<HealthCategory>,
}

fn item(name: &str, status: &'static str, detail: Option<String>) -> HealthItem {
    HealthItem { name: name.to_string(), status, detail }
}

fn system_temperature_is_fahrenheit(config: &sentryusb_config::SetupConfig) -> bool {
    config
        .get("SYSTEM_TEMPERATURE_UNIT")
        .filter(|unit| !unit.is_empty())
        .or_else(|| config.get("TEMPERATURE_UNIT"))
        .is_some_and(|unit| unit.eq_ignore_ascii_case("F"))
}

pub async fn health_check(State(_s): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let mut categories: Vec<HealthCategory> = Vec::new();

    // Config is read first: the temperature display unit comes from it.
    let active_cfg: std::collections::HashMap<String, String> =
        sentryusb_config::parse_file(sentryusb_config::find_config_path())
            .map(|(active, _commented)| active)
            .unwrap_or_default();

    let use_f = system_temperature_is_fahrenheit(&active_cfg);
    let fmt_temp = |celsius: f64| -> String {
        if use_f {
            format!("{:.1}°F", celsius * 9.0 / 5.0 + 32.0)
        } else {
            format!("{:.1}°C", celsius)
        }
    };
    let fmt_threshold = |celsius: f64| -> String {
        if use_f {
            format!("{:.0}°F", celsius * 9.0 / 5.0 + 32.0)
        } else {
            format!("{:.0}°C", celsius)
        }
    };

    // Hardware
    let mut hw = Vec::new();
    let mut cpu_temp_val: Option<f64> = None;
    if let Ok(data) = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp") {
        if let Ok(millideg) = data.trim().parse::<f64>() {
            cpu_temp_val = Some(millideg / 1000.0);
        }
    }
    match cpu_temp_val {
        Some(t) if t >= 80.0 => hw.push(item("CPU temperature", "fail",
            Some(format!("{} (>{})", fmt_temp(t), fmt_threshold(80.0))))),
        Some(t) if t >= 70.0 => hw.push(item("CPU temperature", "warn",
            Some(fmt_temp(t)))),
        Some(t) => hw.push(item("CPU temperature", "pass",
            Some(fmt_temp(t)))),
        None => hw.push(item("CPU temperature", "warn",
            Some("unavailable".to_string()))),
    }
    if let Ok(out) = sentryusb_shell::run("vcgencmd", &["measure_temp"]).await {
        let raw = out.trim().trim_start_matches("temp=").trim_end_matches("'C");
        let detail = match raw.parse::<f64>() {
            Ok(celsius) => fmt_temp(celsius),
            Err(_) => raw.to_string(),
        };
        hw.push(item("GPU temperature", "pass", Some(detail)));
    }
    if let Ok(out) = sentryusb_shell::run("vcgencmd", &["get_throttled"]).await {
        let raw = out.trim().trim_start_matches("throttled=").to_string();
        let val = u64::from_str_radix(raw.trim_start_matches("0x"), 16).unwrap_or(0);
        let now = val & 0x7;
        let past = (val >> 16) & 0x7;
        if now != 0 {
            hw.push(item("Power/throttling", "fail", Some(format!("active: {}", raw))));
        } else if past != 0 {
            hw.push(item("Power/throttling", "warn", Some(format!("past event: {}", raw))));
        } else {
            hw.push(item("Power/throttling", "pass", None));
        }
    }
    // The picker records the active CPU variant at each service start.
    match std::fs::read_to_string("/opt/dashusb/active-variant") {
        Ok(s) => {
            let variant = s.trim().to_string();
            if variant.is_empty() {
                hw.push(item("Binary variant", "warn", Some("active-variant file empty".to_string())));
            } else {
                hw.push(item("Binary variant", "pass", Some(variant)));
            }
        }
        Err(_) => {
            hw.push(item(
                "Binary variant",
                "warn",
                Some("active-variant missing (single-binary layout — re-run install-pi.sh to migrate)".to_string()),
            ));
        }
    }
    categories.push(HealthCategory { name: "Hardware".to_string(), items: hw });

    // Storage
    let mut st = Vec::new();
    let storage = crate::status::managed_storage_health();
    let storage_status = match storage.state {
        "healthy" => "pass", "recovering" => "recovering", "fail" => "fail", "warn" => "warn", _ => "unknown",
    };
    st.push(item("Recording storage", storage_status, Some(storage.message.clone())));
    // Measure index capacity only on the actual writable /mutable mount.
    // Otherwise stat would inspect the root filesystem and report a false pass.
    let mounts = std::fs::read_to_string("/proc/mounts").ok();
    let mutable_mounted = mounts
        .as_deref()
        .map(|m| m.lines().any(|l| l.split_whitespace().nth(1) == Some("/mutable")));
    let mutable_rw: Option<bool> = mounts.as_deref().map(|m| {
        m.lines().any(|line| {
            let mut f = line.split_whitespace();
            let _dev = f.next();
            f.next() == Some("/mutable")
                && f.nth(1).is_some_and(|opts| opts.split(',').any(|o| o == "rw"))
        })
    });
    match (mutable_mounted, mutable_rw) {
        (Some(false), _) => st.push(item(
            "Clip index capacity",
            "fail",
            Some("/mutable is not mounted — clips cannot be indexed or archived".to_string()),
        )),
        (Some(true), Some(false)) => st.push(item(
            "Clip index capacity",
            "fail",
            Some("/mutable is mounted read-only — clips cannot be indexed until it is rw again (filesystem error?)".to_string()),
        )),
        (None, _) => st.push(item(
            "Clip index capacity",
            "warn",
            Some("cannot read /proc/mounts to verify /mutable".to_string()),
        )),
        (Some(true), _) => {
            let stat_out = sentryusb_shell::run(
                "stat", &["--file-system", "--format=%d %c", "/mutable/."],
            ).await;
            let parsed = stat_out.ok().and_then(|out| {
                let parts: Vec<u64> = out
                    .trim()
                    .split_whitespace()
                    .filter_map(|p| p.parse().ok())
                    .collect();
                match parts[..] {
                    [free, total] if total > 0 => Some((free, total)),
                    _ => None,
                }
            });
            match parsed {
                None => st.push(item(
                    "Clip index capacity",
                    "warn",
                    Some("inode statistics unavailable for /mutable".to_string()),
                )),
                Some((free, total)) => {
                    let reserve = sentryusb_gadget::space::inode_reserve(total);
                    let counts = format!("{} of {} inodes free", free, total);
                    if free == 0 {
                        st.push(item("Clip index capacity", "fail", Some(format!(
                            "{} — index is full; new clips cannot be indexed or archived",
                            counts
                        ))));
                    } else if free <= reserve {
                        st.push(item("Clip index capacity", "warn", Some(format!(
                            "{} — below the {} reserve; automatic cleanup should be releasing old snapshots",
                            counts, reserve
                        ))));
                    } else {
                        st.push(item("Clip index capacity", "pass", Some(counts)));
                    }
                }
            }
        }
    }
    let cleanup_status = if std::path::Path::new("/run/dashusb_inode_stall").exists() {
        "fail"
    } else {
        match storage.cleanup_state.as_str() {
            "healthy" | "recovered" => "pass", "recovering" => "recovering", "failed" => "warn", _ => "unknown",
        }
    };
    st.push(item("Automatic storage cleanup", cleanup_status, Some(match cleanup_status {
        "fail" => "Cleanup could not restore clip-index inode headroom. Check storage logs and the filesystem.",
        "warn" => "Cleanup needs attention. Check storage logs for the failed operation.",
        "unknown" => "No recent cleanup heartbeat. Check that the archive service is running.",
        "recovering" => "Releasing old snapshots to restore headroom.",
        _ => "Recording headroom is monitored automatically.",
    }.into())));
    // Ignore disabled optional disk images.
    let user_wants = |size_key: &str| -> bool {
        // Health needs only zero/nonzero; setup validates exact sizes.
        let Some(raw) = active_cfg.get(size_key) else { return false; };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return false;
        }
        let digits: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        digits.parse::<f64>().map(|n| n > 0.0).unwrap_or(false)
    };

    let disks: &[(&str, &str, Option<&str>)] = &[
        // The cam disk is always expected, so a miss is a hard fail.
        ("/backingfiles/cam_disk.bin", "cam disk image", None),
    ];
    for (img, label, size_key) in disks {
        // Skip optional disks the user never asked for.
        if let Some(key) = size_key {
            if !user_wants(key) {
                continue;
            }
        }
        if std::path::Path::new(img).exists() {
            st.push(item(label, "pass", None));
        } else {
            // cam is critical. An optional disk that is configured but
            // missing only warns: setup or archiving went wrong.
            let status = if size_key.is_none() { "fail" } else { "warn" };
            st.push(item(label, status, Some("missing".to_string())));
        }
    }
    // Missing source content makes the recordings bind mount appear empty.
    if std::path::Path::new("/mutable/Recordings").is_dir() {
        st.push(item("Recordings directory", "pass", None));
    } else {
        st.push(item(
            "Recordings directory",
            "fail",
            Some("/mutable/Recordings missing — Samba + web listing will be empty".to_string()),
        ));
    }
    categories.push(HealthCategory { name: "Storage".to_string(), items: st });

    // Runtime scripts must exist and executable entries must retain their mode.
    let mut core = Vec::new();
    let core_files: &[(&str, &str, bool)] = &[
        ("/opt/dashusb/dashusb", "DashUSB binary", true),
        ("/root/bin/archiveloop", "archiveloop script", false),
        ("/root/bin/envsetup.sh", "envsetup.sh", false),
        ("/root/bin/enable_gadget.sh", "enable_gadget.sh", true),
        ("/root/bin/disable_gadget.sh", "disable_gadget.sh", true),
        ("/root/bin/make_snapshot.sh", "make_snapshot.sh", true),
        ("/root/bin/release_snapshot.sh", "release_snapshot.sh", true),
        ("/root/bin/manage_free_space.sh", "manage_free_space.sh", true),
        ("/root/bin/waitforidle", "waitforidle", false),
        ("/root/bin/mountimage", "mountimage", false),
        ("/root/bin/remountfs_rw", "remountfs_rw", false),
    ];
    for (path, label, _must_exec) in core_files {
        match std::fs::metadata(path) {
            Err(_) => core.push(item(label, "fail", Some(format!("{} missing", path)))),
            Ok(_md) => {
                #[cfg(unix)]
                {
                    if *_must_exec {
                        use std::os::unix::fs::PermissionsExt;
                        if _md.permissions().mode() & 0o111 == 0 {
                            core.push(item(label, "warn", Some(format!("{} exists but not executable", path))));
                            continue;
                        }
                    }
                }
                core.push(item(label, "pass", Some(path.to_string())));
            }
        }
    }
    categories.push(HealthCategory { name: "Core Files".to_string(), items: core });

    // Configuration
    let mut cfg = Vec::new();
    let config_path = sentryusb_config::find_config_path();
    if std::path::Path::new(config_path).exists() {
        cfg.push(item("Config file", "pass", Some(config_path.to_string())));
    } else {
        cfg.push(item("Config file", "fail", Some("No dashusb.conf found".to_string())));
    }
    let setup_markers = [
        "/dashusb/DASHUSB_SETUP_FINISHED",
        "/boot/firmware/DASHUSB_SETUP_FINISHED",
        "/boot/DASHUSB_SETUP_FINISHED",
    ];
    let setup_finished = setup_markers.iter().find(|p| std::path::Path::new(p).exists());
    match setup_finished {
        Some(p) => cfg.push(item("Setup finished", "pass", Some(format!("{} exists", p)))),
        None => cfg.push(item(
            "Setup finished",
            "fail",
            Some("DASHUSB_SETUP_FINISHED marker not found".to_string()),
        )),
    }
    match std::fs::read_to_string("/etc/fstab") {
        Err(_) => cfg.push(item("fstab", "fail", Some("Cannot read /etc/fstab".to_string()))),
        Ok(fstab) => {
            cfg.push(item(
                "backingfiles in fstab",
                if fstab.contains("backingfiles") { "pass" } else { "fail" },
                if fstab.contains("backingfiles") { None } else { Some("Missing from /etc/fstab".to_string()) },
            ));
            cfg.push(item(
                "mutable in fstab",
                if fstab.contains("mutable") { "pass" } else { "fail" },
                if fstab.contains("mutable") { None } else { Some("Missing from /etc/fstab".to_string()) },
            ));
            cfg.push(item(
                "cam_disk in fstab",
                if fstab.contains("cam_disk.bin") { "pass" } else { "warn" },
                if fstab.contains("cam_disk.bin") { None } else { Some("Missing (no cam disk configured?)".to_string()) },
            ));
        }
    }
    categories.push(HealthCategory { name: "Configuration".to_string(), items: cfg });

    // Gadget health requires UDC binding and a backed LUN, not just configfs.
    let mut gad = Vec::new();
    if sentryusb_gadget::is_active() {
        gad.push(item("Gadget UDC bound", "pass", None));
        let lun0 = "/sys/kernel/config/usb_gadget/dashusb/functions/mass_storage.0/lun.0/file";
        match std::fs::read_to_string(lun0) {
            Ok(s) if !s.trim().is_empty() => {
                gad.push(item("lun.0 backing file", "pass", Some(s.trim().to_string())));
            }
            _ => gad.push(item(
                "lun.0 backing file",
                "fail",
                Some("gadget is bound but exposes no LUN.0 — car will see the drive but nothing on it".to_string()),
            )),
        }
        // UDC state reflects the car link; configfs binding reflects only intent.
        let udc_state = crate::status::read_udc_state();
        match udc_state.as_str() {
            "configured" => gad.push(item("Host link (UDC state)", "pass", None)),
            "" => {}
            other => gad.push(item(
                "Host link (UDC state)",
                "warn",
                Some(format!(
                    "gadget is bound but the host link reads '{other}' — normal while the car \
                     sleeps or suspends the bus, a problem if the car is awake and recording"
                )),
            )),
        }
    } else if std::path::Path::new("/sys/kernel/config/usb_gadget/dashusb").exists() {
        gad.push(item(
            "Gadget UDC bound",
            "warn",
            Some("configfs dir exists but UDC is empty — toggle drives to re-bind".to_string()),
        ));
    } else {
        gad.push(item("Gadget UDC bound", "warn", Some("gadget disabled".to_string())));
    }
    categories.push(HealthCategory { name: "USB Gadget".to_string(), items: gad });

    // BLE serves companion-app pairing/setup/API proxy, not vehicle data.
    let mut ble = Vec::new();

    // Distinguish app BLE health from the vehicle USB link.
    let ble_running = sentryusb_shell::run(
        "systemctl", &["is-active", "--quiet", "dashusb-ble"],
    ).await.is_ok();
    ble.push(item(
        "App pairing service (iOS/Android)",
        if ble_running { "pass" } else { "warn" },
        if ble_running {
            None
        } else {
            Some(
                "dashusb-ble inactive — phone-app pairing unavailable (does NOT affect \
                 car data)"
                    .to_string(),
            )
        },
    ));
    let dbus_policy = std::path::Path::new("/etc/dbus-1/system.d/com.dashusb.ble.conf").exists();
    ble.push(item(
        "D-Bus policy",
        if dbus_policy { "pass" } else { "warn" },
        if dbus_policy { None } else { Some("com.dashusb.ble.conf missing".to_string()) },
    ));
    categories.push(HealthCategory { name: "BLE".to_string(), items: ble });

    // Check RTC hardware only when the user enabled RTC support.
    let rtc_opted_in = active_cfg.get("RTC_BATTERY_ENABLED").map(|v| v.trim() == "true").unwrap_or(false);
    if rtc_opted_in {
        let mut rtc = Vec::new();
        let has_rtc = std::path::Path::new("/dev/rtc0").exists();
        rtc.push(item(
            "RTC device",
            if has_rtc { "pass" } else { "warn" },
            if has_rtc { None } else { Some("no /dev/rtc0 — clock will reset on power loss".to_string()) },
        ));
        if has_rtc {
            // Pi 5 RTC battery charge level.
            if let Ok(v) = std::fs::read_to_string("/sys/class/rtc/rtc0/device/charging_voltage_now") {
                let uv: i64 = v.trim().parse().unwrap_or(0);
                let mv = uv / 1000;
                let status = if mv >= 2800 { "pass" } else if mv >= 2000 { "warn" } else { "fail" };
                rtc.push(item("RTC battery", status, Some(format!("{} mV", mv))));
            }
        }
        categories.push(HealthCategory { name: "Clock / RTC".to_string(), items: rtc });
    }

    // Archive-loop failure is critical because footage otherwise stays local.
    let mut svcs = Vec::new();
    for (svc, critical) in &[
        ("dashusb", true),
        ("dashusb-archive", true),
        ("avahi-daemon", false),
        ("bluetooth", false),
        ("dashusb-ble", false),
    ] {
        let active = sentryusb_shell::run(
            "systemctl", &["is-active", "--quiet", svc],
        ).await.is_ok();
        let status = if active { "pass" } else if *critical { "fail" } else { "warn" };
        let detail = if active { None } else { Some("inactive".to_string()) };
        svcs.push(item(svc, status, detail));
    }
    categories.push(HealthCategory { name: "Services".to_string(), items: svcs });

    // Network
    let mut net = Vec::new();
    let has_ip = sentryusb_shell::run(
        "bash", &["-c", "ip -4 -o addr show scope global 2>/dev/null | grep -v ' lo ' | head -1"],
    ).await.ok().map(|s| !s.trim().is_empty()).unwrap_or(false);
    net.push(item(
        "Network connectivity",
        if has_ip { "pass" } else { "fail" },
        if has_ip { None } else { Some("no IPv4 address".to_string()) },
    ));
    let dns_ok = sentryusb_shell::run_with_timeout(
        std::time::Duration::from_secs(5),
        "getent", &["hosts", "github.com"],
    ).await.is_ok();
    net.push(item(
        "DNS resolution",
        if dns_ok { "pass" } else { "warn" },
        if dns_ok { None } else { Some("github.com lookup failed".to_string()) },
    ));
    categories.push(HealthCategory { name: "Network".to_string(), items: net });

    // System
    let mut sys = Vec::new();
    if let Ok(data) = std::fs::read_to_string("/proc/uptime") {
        if let Some(secs) = data.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
            let h = (secs / 3600.0) as u64;
            let m = ((secs % 3600.0) / 60.0) as u64;
            sys.push(item("Uptime", "pass", Some(format!("{}h {}m", h, m))));
        }
    }
    let setup_ok = std::path::Path::new("/dashusb/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/firmware/DASHUSB_SETUP_FINISHED").exists()
        || std::path::Path::new("/boot/DASHUSB_SETUP_FINISHED").exists();
    sys.push(item(
        "Setup completed",
        if setup_ok { "pass" } else { "warn" },
        if setup_ok { None } else { Some("setup has not finished".to_string()) },
    ));
    categories.push(HealthCategory { name: "System".to_string(), items: sys });

    // Summary
    let mut fails = 0;
    let mut warns = 0;
    for c in &categories {
        for i in &c.items {
            match i.status {
                "fail" => fails += 1,
                "warn" => warns += 1,
                _ => {}
            }
        }
    }
    let summary = if fails > 0 {
        format!("{} problem{} found", fails, if fails == 1 { "" } else { "s" })
    } else if warns > 0 {
        format!("{} warning{}", warns, if warns == 1 { "" } else { "s" })
    } else if categories.iter().flat_map(|category| &category.items).any(|entry| matches!(entry.status, "unknown" | "recovering")) {
        "No actionable issues reported; some checks are unavailable or recovering".to_string()
    } else {
        "All systems operational".to_string()
    };

    let report = HealthReport { summary, categories };
    (StatusCode::OK, Json(serde_json::to_value(report).unwrap_or_default()))
}

/// POST /api/diagnostics/refresh
pub async fn refresh_diagnostics(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    match gather_diagnostics(state).await {
        Ok(report) => match tokio::fs::write("/tmp/diagnostics.txt", report).await {
            Ok(_) => crate::json_ok(),
            Err(e) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to save diagnostics: {}", e)),
        },
        Err(e) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to generate diagnostics: {}", e)),
    }
}

/// A fresh capture is returned directly so downloads cannot read an older
/// cached report or another request's partially written file.
pub async fn download_diagnostics(State(state): State<AppState>) -> axum::response::Response {
    match gather_diagnostics(state).await {
        Ok(report) => diagnostics_download_response(report),
        Err(e) => crate::json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to capture diagnostics: {}", e)).into_response(),
    }
}

fn diagnostics_download_response(report: String) -> axum::response::Response {
    let filename = format!("attachment; filename=\"dashusb-diagnostics-{}.txt\"", chrono::Utc::now().format("%Y%m%d-%H%M%S-UTC"));
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, filename),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        sanitize_diagnostics(&report),
    ).into_response()
}

async fn gather_diagnostics(state: AppState) -> anyhow::Result<String> {
    let capture_time = chrono::Utc::now().to_rfc3339();
    let script = sentryusb_shell::run_with_timeout(
        std::time::Duration::from_secs(60),
        "bash",
        &["-c", DIAGNOSTICS_SCRIPT],
    );
    // Include the UI's status sample without relying on a loopback proxy,
    // configured HTTP port, or auth cookie. Raw USB probes below are live.
    let status = tokio::time::timeout(std::time::Duration::from_secs(3), crate::status::get_status(State(state)));
    let (report, status) = tokio::join!(script, status);
    let status = match status {
        Ok((code, Json(value))) if code.is_success() => serde_json::to_string_pretty(&value)?,
        _ => "Status sample unavailable (timed out or device busy)".into(),
    };
    Ok(format!("{}\n====== UI status sample (capture started {capture_time}; may be cached) ======\n{status}\n", report?))
}

/// Inline diagnostics gathering script.
const DIAGNOSTICS_SCRIPT: &str = r#"{
  echo "====== DashUSB Diagnostics ======"
  echo "Date: $(date)"
  echo "Hostname: $(hostname)"
  echo "Uptime: $(uptime)"
  echo "Capture started (UTC): $(date -u +%FT%TZ)"
  echo "Capture is read-only; USB drives are not toggled or mounted."
  echo ""

  echo "====== version ======"
  cat /opt/dashusb/version 2>/dev/null || echo "unknown"
  uname -a
  cat /sys/firmware/devicetree/base/model 2>/dev/null; echo
  echo ""

  # Capture volatile USB/power evidence before slower storage/log probes.
  echo "====== USB state and recording activity ======"
  gadget=/sys/kernel/config/usb_gadget/dashusb
  if [ -d "$gadget" ]; then
    for attr in UDC bcdUSB; do
      echo "$attr: $(cat "$gadget/$attr" 2>/dev/null)"
    done
    for cfg in "$gadget"/configs/*; do
      [ -d "$cfg" ] || continue
      echo "$cfg/MaxPower (mA): $(cat "$cfg/MaxPower" 2>/dev/null)"
    done
    for lun in "$gadget"/functions/mass_storage.*/lun.*; do
      [ -d "$lun" ] || continue
      for attr in file ro nofua removable; do
        echo "$lun/$attr: $(cat "$lun/$attr" 2>/dev/null)"
      done
    done
  else
    echo "Gadget configuration absent"
  fi
  usb_sample() {
    echo "Sample UTC: $(date -u +%FT%TZ)"
    for u in /sys/class/udc/*; do
      [ -d "$u" ] || continue
      for attr in state current_speed maximum_speed; do
        echo "$u/$attr: $(cat "$u/$attr" 2>/dev/null)"
      done
    done
    cam=/backingfiles/cam_disk.bin
    if sample=$(timeout 2 stat -c 'size_bytes=%s mtime_epoch=%Y modified=%y' "$cam" 2>/dev/null); then
      echo "$cam: $sample"
      mtime=${sample#*mtime_epoch=}; mtime=${mtime%% *}
      echo "cam_last_write_secs=$(( $(date +%s) - mtime ))"
    else
      echo "cam_disk.bin metadata unavailable"
    fi
    for comm in /proc/[0-9]*/comm; do
      read -r name < "$comm" 2>/dev/null || continue
      case "$name" in
        file-storage*|gadgetwatchdog|kmsgmirror)
          pid=${comm%/comm}; pid=${pid##*/}
          echo "Thread $name (pid $pid)"
          cat "/proc/$pid/io" "/proc/$pid/wchan" 2>/dev/null; echo
          ;;
      esac
    done
  }
  usb_sample
  sleep 2
  usb_sample
  echo "Two samples show activity only during capture; no writes can also mean recording is paused."
  echo ""

  echo "====== power / throttling ======"
  timeout 3 vcgencmd get_throttled 2>&1 || echo "throttling flags unavailable"
  timeout 3 vcgencmd pmic_read_adc 2>&1 || echo "PMIC rail measurements unavailable on this board"
  echo ""

  echo "====== disk / images ======"
  timeout 3 df -h /dashusb/ / /backingfiles/ /mutable/ 2>&1 || echo "capacity probe unavailable or timed out"
  timeout 3 df -i /backingfiles/ /mutable/ 2>&1 || echo "inode probe unavailable or timed out"
  cat /proc/mounts /proc/diskstats 2>/dev/null
  for scheduler in /sys/block/*/queue/scheduler; do
    echo "$scheduler: $(cat "$scheduler" 2>/dev/null)"
  done
  for img in cam; do
    f="/backingfiles/${img}_disk.bin"
    if [ -f "$f" ]; then
      echo "$img disk: $(timeout 2 du -h "$f" 2>/dev/null | cut -f1)"
    fi
  done
  echo ""

  echo "====== gadget stall evidence (latest 3, up to 200 lines each) ======"
  # Generated filenames sort chronologically; never scan unrelated files.
  while IFS= read -r f; do
    [ -f "$f" ] || continue
    echo "--- $f ---"
    timeout 2 tail -200 "$f" 2>&1 || echo "stall evidence unavailable"
  done < <(printf '%s\n' /mutable/gadget_stall_*.log | sort -r | head -3)
  echo ""

  echo "====== persistent kernel history (last 500) ======"
  timeout 2 tail -500 /mutable/kernel.log 2>&1 || echo "no persistent kernel history available"
  echo ""

  echo "====== storage cleanup state ======"
  cat /run/dashusb_storage_cleanup.json 2>/dev/null || echo "cleanup state unavailable"
  [ ! -e /run/dashusb_inode_stall ] || echo "Clip index inode stall flag present"
  echo ""

  echo "====== network ======"
  ip -4 addr show 2>/dev/null | grep inet || ifconfig 2>/dev/null
  echo ""

  echo "====== services ======"
  for svc in dashusb dashusb-archive avahi-daemon; do
    status=$(systemctl is-active "$svc" 2>/dev/null || echo "not found")
    echo "  $svc: $status"
  done
  echo ""

  echo "====== archiveloop ======"
  # Bounded, but wide enough to show a failure repeating across several
  # archive cycles rather than a single truncated window.
  timeout 2 tail -1000 /mutable/archiveloop.log 2>/dev/null || echo "no archiveloop log"
  echo ""

  echo "====== dashusb service journal (last 300) ======"
  journalctl -u dashusb -n 300 --no-pager 2>/dev/null \
    || echo "no dashusb journal entries"
  echo ""

  echo "====== temperatures ======"
  cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null | awk '{printf "CPU: %.1f°C\n", $1/1000}'
  vcgencmd measure_temp 2>/dev/null || true
  echo ""

  echo "====== dmesg (last 200) ======"
  dmesg -T 2>/dev/null | tail -200
  echo ""

  echo "Capture completed (UTC): $(date -u +%FT%TZ)"
  echo "====== end of diagnostics ======"
} 2>&1"#;

/// GET /api/diagnostics
pub async fn get_diagnostics(State(_s): State<AppState>) -> impl IntoResponse {
    match std::fs::read_to_string("/tmp/diagnostics.txt") {
        Ok(data) => {
            let cleaned = sanitize_diagnostics(&data);
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                cleaned,
            )
        }
        Err(_) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Diagnostics have not been generated yet.\nClick the Refresh button above to generate a diagnostics report.".to_string(),
        ),
    }
}

fn sanitize_diagnostics(raw: &str) -> String {
    let ansi_re = regex::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    let cleaned = ansi_re.replace_all(raw, "");

    // Preserve text formatting controls only.
    cleaned
        .chars()
        .filter(|&c| c == '\t' || c == '\n' || c == '\r' || c >= '\x20')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::system_temperature_is_fahrenheit;

    #[tokio::test]
    async fn fresh_download_is_a_timestamped_uncached_text_attachment() {
        let response = super::diagnostics_download_response("fresh USB capture\n\x1b[31mconfigured\x1b[0m\x00".into());
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers[axum::http::header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(headers[axum::http::header::CACHE_CONTROL], "no-store");
        let disposition = headers[axum::http::header::CONTENT_DISPOSITION].to_str().unwrap();
        assert!(disposition.starts_with("attachment; filename=\"dashusb-diagnostics-"));
        assert!(disposition.ends_with("-UTC.txt\""));
        let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"fresh USB capture\nconfigured");
    }

    #[test]
    fn system_temperature_override_takes_priority_over_measurement_system() {
        for (overall, system, expected_fahrenheit) in [("F", "C", false), ("C", "F", true)] {
            let config = sentryusb_config::SetupConfig::from([
                ("TEMPERATURE_UNIT".into(), overall.into()),
                ("SYSTEM_TEMPERATURE_UNIT".into(), system.into()),
            ]);
            assert_eq!(system_temperature_is_fahrenheit(&config), expected_fahrenheit);
        }
    }

    #[test]
    fn unset_system_temperature_inherits_overall_unit_or_celsius_default() {
        let mut config = sentryusb_config::SetupConfig::new();
        assert!(!system_temperature_is_fahrenheit(&config));
        config.insert("TEMPERATURE_UNIT".into(), "F".into());
        assert!(system_temperature_is_fahrenheit(&config));
        config.insert("SYSTEM_TEMPERATURE_UNIT".into(), String::new());
        assert!(system_temperature_is_fahrenheit(&config));
    }

    #[test]
    fn commented_system_temperature_does_not_override_active_unit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dashusb.conf");
        std::fs::write(&path, "export TEMPERATURE_UNIT=F\n#export SYSTEM_TEMPERATURE_UNIT=C\n").unwrap();
        let (active, _) = sentryusb_config::parse_file(path.to_str().unwrap()).unwrap();
        assert!(system_temperature_is_fahrenheit(&active));
    }
}
