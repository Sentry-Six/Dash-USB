//! Once-per-version support-file refresh and idempotent configuration-key migration.

use std::time::Duration;

use tracing::{info, warn};

const VERSION_FILE: &str = "/opt/dashusb/version";
const MIGRATE_DIR: &str = "/opt/dashusb";
/// Ref used by the generated migration script for its support files.
const USED_REF_FILE: &str = "/opt/dashusb/.migrate-used-ref";

pub async fn run_startup_migration() {
    // Heals must run outside the per-version marker so later additions apply.
    heal_temperature_unit_key();

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

    // Prefer the exact version tag so the refreshed scripts match the
    // installed binary; the configured branch is only the fallback ref.
    let source = sentryusb_config::github_source();
    let script_ref = if current_version == "unknown" {
        source.branch.clone()
    } else {
        current_version.clone()
    };
    let tarball_url = format!(
        "https://github.com/{}/archive/{}.tar.gz",
        source.repo_slug, script_ref
    );
    let fallback_url = format!(
        "https://github.com/{}/archive/{}.tar.gz",
        source.repo_slug, source.branch
    );
    // Default devices keep tag-matched helpers; explicit branches track their
    // branch helper.
    let patches_url = if source.branch_explicit {
        tracing::info!(
            "[migrate] explicit BRANCH={} set — support files track that branch",
            source.branch
        );
        format!(
            "https://raw.githubusercontent.com/{}/{}/setup/pi/apply-runtime-patches.sh",
            source.repo_slug, source.branch
        )
    } else {
        String::new()
    };
    // The script reports the actual ref if its fallback changes this guess.
    let mut effective_ref = if source.branch_explicit {
        source.branch.clone()
    } else {
        script_ref.clone()
    };
    let _ = std::fs::remove_file(USED_REF_FILE);

    // Pass validated config values as arguments, never interpolated shell.
    let script = build_migration_script();

    // Retry transient boot-time DNS/network failures. Script writes are
    // idempotent, so a partial first attempt is safe to repeat.
    let mut last_err: Option<String> = None;
    for attempt in 1..=3 {
        match sentryusb_shell::run_with_timeout(
            Duration::from_secs(180),
            "bash",
            &[
                "-c",
                &script,
                "dashusb-migrate",
                &tarball_url,
                &fallback_url,
                &patches_url,
                &script_ref,
                &source.branch,
                USED_REF_FILE,
            ],
        )
        .await
        {
            Ok(_) => {
                // Fail closed unless the script reports its actual ref; mixing
                // a branch helper with a tag ref can select incompatible files.
                let reported = std::fs::read_to_string(USED_REF_FILE)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let _ = std::fs::remove_file(USED_REF_FILE);
                let Some(reported) = reported else {
                    warn!(
                        "[migrate] migration reported success but wrote no ref to {} — not marking \
                         migrated; will retry on next boot",
                        USED_REF_FILE
                    );
                    return;
                };
                if reported != effective_ref {
                    info!(
                        "[migrate] support files came from {} (not {}) — using it for DASHUSB_REF",
                        reported, effective_ref
                    );
                    effective_ref = reported;
                }

                // Migration replaces BLE files, so reapply idempotent,
                // hardware-gated runtime patches afterward. Missing helpers
                // are restored by the next OTA update.
                if std::path::Path::new("/usr/local/bin/dashusb-apply-runtime-patches").exists() {
                    match sentryusb_shell::run_with_timeout(
                        Duration::from_secs(30),
                        "env",
                        &[
                            &format!("DASHUSB_REPO_SLUG={}", source.repo_slug),
                            // Match the ref from which the helper was installed.
                            &format!("DASHUSB_REF={}", effective_ref),
                            "/usr/local/bin/dashusb-apply-runtime-patches",
                        ],
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
                // Retrying permanent archive or permission errors only delays boot.
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
    // Absence of the marker retries the migration next boot.
}

/// Merge the retired `SYSTEM_TEMPERATURE_UNIT` into `TEMPERATURE_UNIT`, with
/// the specific override taking precedence. Idempotent after the old key is
/// commented out.
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

fn build_migration_script() -> String {
    // Arguments: tag and fallback URLs, optional patch URL, tag and branch
    // refs, then the path where the selected ref is reported.
    format!(
        r#"set -e
TARBALL_URL="$1"
FALLBACK_URL="$2"
PATCHES_URL="$3"
TAG_REF="$4"
BRANCH_REF="$5"
USED_REF_FILE="$6"
# Assume the tag until a fallback actually fires. The caller runs the helper
# with this ref, so guessing wrong yields a branch helper fed tag payloads.
USED_REF="$TAG_REF"

# Remount filesystem as read-write (no-op if already rw)
/root/bin/remountfs_rw 2>/dev/null || mount -o remount,rw / 2>/dev/null || true

# Ensure /root/bin exists — on fresh Rust installs it isn't created by setup,
# so cp targets below would otherwise fail.
mkdir -p /root/bin

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

# Download repo tarball — try version tag first, fall back to tracking branch
if ! curl -fsSL "$TARBALL_URL" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null; then
  curl -fsSL "$FALLBACK_URL" | tar xz --strip-components=1 -C "$TMPDIR" 2>/dev/null || exit 1
  # The tag tarball lost; everything unpacked below is branch content.
  USED_REF="$BRANCH_REF"
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

# ── Refresh the runtime-patches helper ──
# $PATCHES_URL is non-empty only when the conf explicitly sets BRANCH; then
# the branch copy wins so the updater's selection survives this migration.
# Default devices take the tag-tarball copy, which matches their binary.
# Without either, existing installs would re-run the OLD on-disk version.
PATCHES_STAGE="$TMPDIR/patches.new"
if [ -n "$PATCHES_URL" ] \
   && curl -fsSL --max-time 15 -o "$PATCHES_STAGE" "$PATCHES_URL" 2>/dev/null \
   && [ -s "$PATCHES_STAGE" ] && bash -n "$PATCHES_STAGE" 2>/dev/null; then
  install -m 755 "$PATCHES_STAGE" /usr/local/bin/dashusb-apply-runtime-patches
  # Explicit-BRANCH copy won, whatever the tarball was.
  USED_REF="$BRANCH_REF"
elif [ -f "$TMPDIR/setup/pi/apply-runtime-patches.sh" ]; then
  install -m 755 "$TMPDIR/setup/pi/apply-runtime-patches.sh" /usr/local/bin/dashusb-apply-runtime-patches
fi

# Report the ref the helper actually came from so the caller runs it with a
# matching DASHUSB_REF instead of assuming the tag. NOT best-effort: if this
# write is lost, the caller keeps its tag guess and can run a branch helper
# with the tag ref. `set -e` turns a failure here into a failed migration,
# which is retried.
printf '%s' "$USED_REF" > "$USED_REF_FILE"

# ── Restart the phone-app BLE daemon ──
systemctl enable dashusb-ble 2>/dev/null || true
systemctl restart dashusb-ble 2>/dev/null || true
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the shell template and verify its syntax.
    #[test]
    fn migration_script_parses() {
        let script = build_migration_script();
        // Config-derived URLs must arrive only as positional arguments.
        assert!(!script.contains("{repo}"), "unsubstituted {{repo}}");
        assert!(!script.contains("{branch}"), "unsubstituted {{branch}}");
        assert!(!script.contains("{tarball_url}"), "unsubstituted {{tarball_url}}");
        assert!(script.contains("TARBALL_URL=\"$1\""), "primary URL must come from $1");
        assert!(script.contains("FALLBACK_URL=\"$2\""), "fallback URL must come from $2");
        assert!(script.contains("PATCHES_URL=\"$3\""), "patches URL must come from $3");
        assert!(script.contains("TAG_REF=\"$4\""), "tag ref must come from $4");
        assert!(script.contains("BRANCH_REF=\"$5\""), "branch ref must come from $5");
        assert!(
            script.contains("USED_REF_FILE=\"$6\""),
            "used-ref report path must come from $6"
        );
        // The caller must receive the helper's actual source ref.
        assert!(
            script.contains("> \"$USED_REF_FILE\""),
            "script must report the ref it actually installed from"
        );

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
