//! Startup migration: refresh peripheral files (shell scripts, BLE daemon,
//! Avahi service, service units) when a replace-only binary update has left
//! the surrounding on-disk artifacts at the previous version.
//!
//! Gated by the marker file `/opt/dashusb/.migrated-<version>`, so it runs at
//! most once per installed version. Never touches user setup configuration.

use std::time::Duration;

use tracing::{info, warn};

const VERSION_FILE: &str = "/opt/dashusb/version";
const MIGRATE_DIR: &str = "/opt/dashusb";
const MIGRATE_REPO: &str = "Sentry-Six/Dash-USB";
const MIGRATE_BRANCH: &str = "main";

pub async fn run_startup_migration() {
    // Unconditional idempotent heals run first. They MUST NOT sit behind the
    // .migrated-<version> marker below: a heal added after a version was
    // already marked would never run.
    heal_temperature_unit_key();

    // Skip in dev mode (no version file, or explicit "dev")
    let current_version = match tokio::fs::read_to_string(VERSION_FILE).await {
        Ok(v) => v.trim().to_string(),
        Err(_) => return,
    };
    if current_version.is_empty() || current_version == "dev" {
        return;
    }

    let marker_file = format!("{}/.migrated-{}", MIGRATE_DIR, current_version);
    if tokio::fs::metadata(&marker_file).await.is_ok() {
        return;
    }

    info!("[migrate] Running startup migration for {}...", current_version);

    // Prefer the exact version tag; fall back to the tracking branch if missing.
    let script_ref = if current_version == "unknown" {
        MIGRATE_BRANCH.to_string()
    } else {
        current_version.clone()
    };
    let tarball_url = format!(
        "https://github.com/{}/archive/{}.tar.gz",
        MIGRATE_REPO, script_ref
    );

    let script = build_migration_script(&tarball_url);

    // Retry up to 3 times, backing off a further 5 s each attempt. The script
    // fails fast on `curl: Could not resolve host: github.com`, exactly the
    // state hit when racing network-online.target at boot. Each retry is a
    // full script run; `set -e` plus idempotent file writes make re-running
    // after a partial success safe (files are overwritten with identical
    // tarball bytes).
    //
    // The service unit's `nss-lookup.target` dependency is the primary fix.
    // This covers the resolver coming up but failing its first query against
    // a cold upstream DNS cache.
    let mut last_err: Option<String> = None;
    for attempt in 1..=3 {
        match sentryusb_shell::run_with_timeout(
            Duration::from_secs(180),
            "bash",
            &["-c", &script],
        )
        .await
        {
            Ok(_) => {
                // Re-apply runtime patches AFTER the migration. The migration
                // script unconditionally rewrites /root/bin/dashusb-ble.py
                // from the upstream tarball, silently undoing board-specific
                // fixes the OTA updater already applied. BCM4345C0
                // non-fatal-adv on Rock 4C+ is the headline case: OTA patches
                // ble.py → reboot → this migration unpatches it → BLE crash
                // loop on next start. The standalone runtime-patches script is
                // idempotent and detection-gated, so the patches survive every
                // migration. Best-effort: a missing script only logs, and the
                // OTA bootstrap path populates it on the next update.
                if std::path::Path::new("/usr/local/bin/dashusb-apply-runtime-patches").exists() {
                    match sentryusb_shell::run_with_timeout(
                        Duration::from_secs(30),
                        "/usr/local/bin/dashusb-apply-runtime-patches",
                        &[],
                    )
                    .await
                    {
                        Ok(_) => info!("[migrate] runtime-patches re-applied post-migration"),
                        Err(e) => warn!(
                            "[migrate] runtime-patches post-migration run failed: {} — BLE pairing may be broken on Rock 4C+",
                            e
                        ),
                    }
                } else {
                    info!("[migrate] runtime-patches script not present (pre-bootstrap install) — skipping; OTA path will populate it");
                }

                let _ = tokio::fs::create_dir_all(MIGRATE_DIR).await;
                if let Err(e) = tokio::fs::write(&marker_file, b"migrated\n").await {
                    warn!("[migrate] Failed to write marker {}: {}", marker_file, e);
                }
                info!("[migrate] Startup migration complete for {}", current_version);
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                // Retry only on transient failure signatures. A 404 on the
                // tarball URL, a write permission error, or a corrupt archive
                // will not fix itself on a second try, and retrying burns 30+
                // seconds of boot on a guaranteed failure.
                let transient = msg.contains("Could not resolve host")
                    || msg.contains("Temporary failure in name resolution")
                    || msg.contains("Connection timed out")
                    || msg.contains("Network is unreachable");
                if attempt < 3 && transient {
                    let wait = Duration::from_secs(5 * attempt as u64);
                    warn!(
                        "[migrate] Startup migration attempt {}/3 hit a transient failure ({}); retrying in {:?}",
                        attempt, msg, wait
                    );
                    tokio::time::sleep(wait).await;
                    last_err = Some(msg);
                    continue;
                }
                last_err = Some(msg);
                break;
            }
        }
    }
    warn!(
        "[migrate] Warning: startup migration failed after retries: {}",
        last_err.as_deref().unwrap_or("unknown")
    );
    // No marker written: retry on the next boot.
}

/// Collapse the retired `SYSTEM_TEMPERATURE_UNIT` override into
/// `TEMPERATURE_UNIT`. Dash USB has exactly one temperature source (the Pi
/// CPU); the two-key split is residue from the upstream Tesla project and let
/// the Settings sub-toggle and the alert monitor disagree across a reboot. The
/// specific override wins over the general key, being the newer and more
/// deliberate choice wherever both were set. Idempotent: after the first pass
/// the old key is commented out and this is a no-op.
fn heal_temperature_unit_key() {
    let path = sentryusb_config::find_config_path();
    let Ok((mut active, _)) = sentryusb_config::parse_file(path) else {
        return;
    };
    let Some(sys) = active.remove("SYSTEM_TEMPERATURE_UNIT") else {
        return;
    };
    active.insert("TEMPERATURE_UNIT".to_string(), sys.to_uppercase());
    match sentryusb_config::write_file(path, &active) {
        Ok(()) => info!("[migrate] merged SYSTEM_TEMPERATURE_UNIT into TEMPERATURE_UNIT"),
        Err(e) => tracing::warn!("[migrate] temperature-unit key heal failed: {}", e),
    }
}

fn build_migration_script(tarball_url: &str) -> String {
    format!(
        r#"set -e

# Remount filesystem as read-write (no-op if already rw)
/root/bin/remountfs_rw 2>/dev/null || mount -o remount,rw / 2>/dev/null || true

# Ensure /root/bin exists — on fresh Rust installs it isn't created by setup,
# so cp targets below would otherwise fail.
mkdir -p /root/bin

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

# Download repo tarball — try version tag first, fall back to tracking branch
if ! curl -fsSL "{tarball_url}" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null; then
  FALLBACK="https://github.com/{repo}/archive/{branch}.tar.gz"
  curl -fsSL "$FALLBACK" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null || exit 1
fi

# ── Update run/ scripts ──
if [ -d "$TMPDIR/run" ]; then
  for f in "$TMPDIR"/run/*; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    cp "$f" "/root/bin/$name"
    chmod +x "/root/bin/$name"
  done
fi

# ── Update archive module scripts ──
ARCHIVE_SYSTEM=""
for conf in /root/dashusb.conf /dashusb/dashusb.conf; do
  if [ -f "$conf" ]; then
    ARCHIVE_SYSTEM=$(grep -m1 'ARCHIVE_SYSTEM=' "$conf" 2>/dev/null | tail -1 | sed "s/.*ARCHIVE_SYSTEM=//;s/['\"]//g;s/#.*//" | tr -d ' ') || true
    [ -n "$ARCHIVE_SYSTEM" ] && break
  fi
done
if [ -n "$ARCHIVE_SYSTEM" ]; then
  subdir="${{ARCHIVE_SYSTEM}}_archive"
  if [ -d "$TMPDIR/run/$subdir" ]; then
    for f in "$TMPDIR/run/$subdir"/*; do
      [ -f "$f" ] || continue
      name=$(basename "$f")
      cp "$f" "/root/bin/$name"
      chmod +x "/root/bin/$name"
    done
  fi
fi

# ── Update setup-dashusb (kept as compatibility wrapper) ──
if [ -f "$TMPDIR/setup/pi/setup-dashusb" ]; then
  cp "$TMPDIR/setup/pi/setup-dashusb" "/root/bin/setup-dashusb"
  chmod +x "/root/bin/setup-dashusb"
fi

# ── Update envsetup.sh (kept as compatibility wrapper) ──
if [ -f "$TMPDIR/setup/pi/envsetup.sh" ]; then
  cp "$TMPDIR/setup/pi/envsetup.sh" "/root/bin/envsetup.sh"
  chmod +x "/root/bin/envsetup.sh"
fi

# ── Update BLE peripheral daemon (binary and/or Python fallback) ──
if [ -f "$TMPDIR/server/ble/dashusb-ble.py" ]; then
  cp "$TMPDIR/server/ble/dashusb-ble.py" "/root/bin/dashusb-ble.py"
  chmod +x "/root/bin/dashusb-ble.py"
fi
if [ -f "$TMPDIR/server/ble/dashusb-ble.service" ]; then
  cp "$TMPDIR/server/ble/dashusb-ble.service" "/etc/systemd/system/dashusb-ble.service"
  systemctl daemon-reload
fi

# ── Update the BLE dbus policy alongside the daemon ──
if [ -f "$TMPDIR/server/ble/com.dashusb.ble.conf" ]; then
  cp "$TMPDIR/server/ble/com.dashusb.ble.conf" "/etc/dbus-1/system.d/com.dashusb.ble.conf"
fi

# ── Install BLE Python dependencies if missing ──
# pi-bluetooth/rfkill are Pi-OS niceties: best-effort (absent on other
# distros, usually preinstalled on Pi OS — cheap insurance either way).
for pkg in python3-dbus python3-gi bluez pi-bluetooth rfkill; do
  if ! dpkg-query -W --showformat='${{db:Status-Status}}\n' "$pkg" 2>/dev/null | grep -q '^installed$'; then
    DEBIAN_FRONTEND=noninteractive apt-get -y install "$pkg" 2>/dev/null || true
  fi
done

# ── Configure bluetoothd --experimental by BlueZ version ──
# Legacy BlueZ (< 5.55) needs it for LEAdvertisingManager1; newer BlueZ does not, and
# the flag there registers LE Audio services that trigger Android pairing prompts.
BTOVERRIDE=/etc/systemd/system/bluetooth.service.d/dashusb-experimental.conf
BTDAEMON=$(systemctl cat bluetooth.service 2>/dev/null | grep '^ExecStart=' | head -1 | sed 's/ExecStart=//' | awk '{{print $1}}')
BTDAEMON=${{BTDAEMON:-$(command -v bluetoothd 2>/dev/null)}}
if [ -n "$BTDAEMON" ] && [ -x "$BTDAEMON" ]; then
  BTVER=$("$BTDAEMON" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+' | head -1)
  BTMAJ=${{BTVER%%.*}}
  BTMIN=${{BTVER##*.}}
  if [ -n "$BTVER" ] && {{ [ "$BTMAJ" -lt 5 ] || {{ [ "$BTMAJ" -eq 5 ] && [ "$BTMIN" -lt 55 ]; }}; }}; then
    if [ ! -f "$BTOVERRIDE" ]; then
      mkdir -p /etc/systemd/system/bluetooth.service.d
      cat > "$BTOVERRIDE" << BTEOF
[Service]
ExecStart=
ExecStart=$BTDAEMON --experimental
BTEOF
      systemctl daemon-reload
      systemctl restart bluetooth 2>/dev/null || true
      sleep 2
    fi
  elif [ -f "$BTOVERRIDE" ]; then
    rm -f "$BTOVERRIDE"
    systemctl daemon-reload
    systemctl restart bluetooth 2>/dev/null || true
    sleep 2
  fi
fi

# ── Install/update Avahi mDNS service ──
if [ -f "$TMPDIR/setup/pi/avahi-dashusb.service" ]; then
  if ! dpkg -s avahi-daemon >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -qq avahi-daemon avahi-utils >/dev/null 2>&1 || true
  fi
  if dpkg -s avahi-daemon >/dev/null 2>&1; then
    mkdir -p /etc/avahi/services
    cp "$TMPDIR/setup/pi/avahi-dashusb.service" /etc/avahi/services/dashusb.service
    # IPv4-only mDNS advertising (stale-AAAA slowness + Chrome PNA "CORS"
    # blocks on http://dashusb.local — see the helper's header comment).
    # Nonfatal, but loud: the setup repair path is the self-heal backstop.
    if [ -f "$TMPDIR/setup/pi/avahi-ipv4-only.sh" ]; then
      bash "$TMPDIR/setup/pi/avahi-ipv4-only.sh" \
        || echo "migrate WARNING: could not apply IPv4-only avahi config" >&2
    else
      echo "migrate WARNING: avahi-ipv4-only.sh missing from update payload" >&2
    fi
    systemctl enable avahi-daemon 2>/dev/null || true
    systemctl restart avahi-daemon 2>/dev/null || true
  fi
fi

# ── Keep the WiFi AP from autoconnecting (it is setup-managed) ──
if nmcli -t con show DASHUSB_AP &>/dev/null; then
  nmcli con modify DASHUSB_AP connection.autoconnect no 2>/dev/null || true
fi

# ── Refresh the per-CPU binary picker ──
# The picker now also re-validates the telemetry + ble-action symlinks
# under /root/bin at every boot (the issue #88 SIGILL self-heal). Ship
# the new version to existing installs from the tarball. Harmless on
# very old installs whose dashusb.service predates the ExecStartPre
# hook — the script just sits unused until the unit is updated.
if [ -f "$TMPDIR/pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary" ]; then
  install -m 755 "$TMPDIR/pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary" /usr/local/bin/dashusb-pick-binary
fi

# ── Refresh the runtime-patches helper from the tarball ──
# Without this, new patches we add to apply-runtime-patches.sh never reach
# existing installs — the OTA invocation (the Rust caller after this shell
# script exits) would re-run the OLD on-disk version. Bootstrap pre-v3.11
# installs that never had the helper at all (the file just appears).
if [ -f "$TMPDIR/setup/pi/apply-runtime-patches.sh" ]; then
  install -m 755 "$TMPDIR/setup/pi/apply-runtime-patches.sh" /usr/local/bin/dashusb-apply-runtime-patches
fi

# ── Restart the phone-app BLE daemon ──
systemctl enable dashusb-ble 2>/dev/null || true
systemctl restart dashusb-ble 2>/dev/null || true
"#,
        tarball_url = tarball_url,
        repo = MIGRATE_REPO,
        branch = MIGRATE_BRANCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The migration script is a format!() template full of shell: a stray
    /// unescaped `{` or a typo'd quote renders a script that fails at runtime
    /// on every user's Pi. Render it and let bash parse it.
    #[test]
    fn migration_script_parses() {
        let script = build_migration_script(
            "https://github.com/Sentry-Six/Dash-USB/archive/v0.0.0.tar.gz",
        );
        // Placeholders must all have been substituted.
        assert!(!script.contains("{repo}"), "unsubstituted {{repo}}");
        assert!(!script.contains("{branch}"), "unsubstituted {{branch}}");
        assert!(!script.contains("{tarball_url}"), "unsubstituted {{tarball_url}}");

        let dir = std::env::temp_dir().join("dashusb-migrate-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("migration.sh");
        std::fs::write(&path, &script).unwrap();
        let status = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .status()
            .expect("bash not available");
        assert!(status.success(), "bash -n rejected the migration script");
    }
}
