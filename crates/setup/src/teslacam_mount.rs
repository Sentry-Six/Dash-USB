//! Recordings web mount wiring.
//!
//! Bind-mounts /mutable/Recordings at /var/www/html/Recordings, where the Axum
//! server's ServeDir route reads from. The cttseraser FUSE layer this replaced
//! is no longer shipped, so a legacy `mount.ctts#` fstab entry is detected and
//! removed on the first run after upgrade and pre-bind-mount installs migrate
//! cleanly.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::SetupEmitter;

/// Canonical fstab entry that bind-mounts the recordings source tree at
/// the path the Axum ServeDir route reads from.
const FSTAB_BIND_LINE: &str =
    "/mutable/Recordings /var/www/html/Recordings none bind,nofail,x-systemd.requires=/mutable 0 0";

pub async fn configure_web_mount(emitter: &SetupEmitter) -> Result<bool> {
    // Idempotency: nothing to do once the canonical bind entry is present and
    // no legacy cttseraser entry remains.
    let fstab = std::fs::read_to_string("/etc/fstab").unwrap_or_default();
    let fstab_has_bind = fstab.lines().any(|l| {
        !l.trim_start().starts_with('#')
            && l.contains("/mutable/Recordings")
            && l.contains("/var/www/html/Recordings")
            && l.contains("bind")
    });
    let fstab_has_legacy = fstab
        .lines()
        .any(|l| !l.trim_start().starts_with('#') && l.contains("mount.ctts#"));

    if fstab_has_bind && !fstab_has_legacy {
        return Ok(false);
    }

    emitter.begin_phase("web_mount", "Recordings mount");
    emitter.progress("configuring web (DashUSB mode)");

    // Install runtime packages for the network status APIs. The bind mount
    // itself requires no userspace tooling beyond `mount(8)` (built-in).
    crate::apt::apt_install(
        |m| emitter.progress(m),
        &["net-tools", "wireless-tools", "ethtool"],
        Duration::from_secs(300),
    ).await.context("failed to install networking runtime packages")?;

    // DashUSB owns port 80, so nginx must be stopped and disabled.
    if sentryusb_shell::run("systemctl", &["is-active", "--quiet", "nginx"]).await.is_ok() {
        let _ = sentryusb_shell::run("systemctl", &["stop", "nginx"]).await;
    }
    if sentryusb_shell::run("systemctl", &["is-enabled", "--quiet", "nginx"]).await.is_ok() {
        let _ = sentryusb_shell::run("systemctl", &["disable", "nginx"]).await;
    }

    std::fs::create_dir_all("/mutable/Recordings")?;
    std::fs::create_dir_all("/var/www/html/Recordings")?;

    // Replace any legacy cttseraser entry with the bind-mount entry, then
    // clear systemd's cached failed state so the unit activates immediately
    // (without requiring a reboot on upgrade).
    install_bind_mount_fstab()?;
    let _ = sentryusb_shell::run("systemctl", &["daemon-reload"]).await;
    let _ = sentryusb_shell::run(
        "systemctl",
        &["reset-failed", "var-www-html-Recordings.mount"],
    ).await;
    let _ = sentryusb_shell::run(
        "systemctl",
        &["start", "var-www-html-Recordings.mount"],
    ).await;

    // Samba reads /mutable/Recordings directly and needs nothing here.

    emitter.progress("done configuring web");
    Ok(true)
}

/// Strip any existing Recordings mount entry (or a stale prior bind)
/// from /etc/fstab and add the canonical bind-mount entry.
fn install_bind_mount_fstab() -> Result<()> {
    let content = std::fs::read_to_string("/etc/fstab").unwrap_or_default();
    let kept: Vec<&str> = content
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            if t.starts_with('#') {
                return true;
            }
            // Drop any existing line that targets /var/www/html/Recordings.
            !l.contains("/var/www/html/Recordings")
        })
        .collect();
    let mut new = kept.join("\n");
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(FSTAB_BIND_LINE);
    new.push('\n');
    std::fs::write("/etc/fstab", new)?;
    Ok(())
}
