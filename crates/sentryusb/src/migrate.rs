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
/// Where the migration script reports the ref its support files actually came
/// from. Written by the shell, read back here; see `build_migration_script`.
const USED_REF_FILE: &str = "/opt/dashusb/.migrate-used-ref";

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
    // Empty unless the user explicitly chose a branch: a default device
    // must keep the tag-tarball copy of the patches helper, matching its
    // binary, instead of tracking the default branch's tip.
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
    // Best guess at the ref the helper will come from. The script overwrites
    // this via USED_REF_FILE if a fallback actually fires, because only the
    // shell knows whether the tag tarball or the branch tarball won.
    let mut effective_ref = if source.branch_explicit {
        source.branch.clone()
    } else {
        script_ref.clone()
    };
    let _ = std::fs::remove_file(USED_REF_FILE);

    // Config-derived values reach the script as positional arguments, never
    // interpolated into shell source. The resolver also charset-validates
    // them; both guards stay.
    let script = build_migration_script();

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
                // Adopt the ref the script reports it actually installed from.
                // Only the shell knows whether the tag tarball or the branch
                // fallback won, and running a branch helper with DASHUSB_REF
                // set to the tag is the mixed-source state this pinning
                // exists to prevent.
                if let Ok(reported) = std::fs::read_to_string(USED_REF_FILE) {
                    let reported = reported.trim();
                    if !reported.is_empty() && reported != effective_ref {
                        info!(
                            "[migrate] support files came from {} (not {}) — using it for DASHUSB_REF",
                            reported, effective_ref
                        );
                        effective_ref = reported.to_string();
                    }
                }
                let _ = std::fs::remove_file(USED_REF_FILE);

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
                        "env",
                        &[
                            &format!("DASHUSB_REPO_SLUG={}", source.repo_slug),
                            // The effective ref, matching the one the helper
                            // was installed from above (and update.rs's
                            // patches_ref): the version tag on a default
                            // device, the configured branch only when the
                            // user explicitly set one. Passing the branch
                            // unconditionally made a tag-pinned device fetch
                            // support files from the branch tip post-reboot.
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

fn build_migration_script() -> String {
    // $1 = primary tarball URL (version tag), $2 = fallback tarball URL
    // (configured branch), $3 = raw URL of apply-runtime-patches.sh on the
    // configured branch, $4 = tag ref, $5 = branch ref, $6 = path to write
    // the ref the helper was ACTUALLY installed from.
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
# matching DASHUSB_REF instead of assuming the tag.
printf '%s' "$USED_REF" > "$USED_REF_FILE" 2>/dev/null || true

# ── Restart the phone-app BLE daemon ──
systemctl enable dashusb-ble 2>/dev/null || true
systemctl restart dashusb-ble 2>/dev/null || true
"#
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
        let script = build_migration_script();
        // No config-derived text may be interpolated into shell source: the
        // URLs arrive as positional arguments only.
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
        // The caller runs the helper with whatever this reports, so a script
        // that never writes it would silently keep the tag guess.
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
