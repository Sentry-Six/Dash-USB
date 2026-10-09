//! On-demand snapshot mounts through autofs and per-snapshot symlinks.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::SetupEmitter;

/// Run the automount configuration phase. Silent no-op when autofs is
/// already installed and the map file is in place.
pub async fn configure_automount(emitter: &SetupEmitter) -> Result<bool> {
    let already_configured = sentryusb_shell::run("which", &["automount"]).await.is_ok()
        && Path::new("/etc/auto.master.d/dashusb.autofs").exists();
    if already_configured {
        return Ok(false);
    }

    emitter.begin_phase("automount", "Snapshot automount");
    emitter.progress("Installing autofs...");

    crate::apt::apt_install(
        |m| emitter.progress(m),
        &["autofs"],
        Duration::from_secs(300),
    ).await.context("failed to install autofs")?;

    // The Raspbian Stretch autofs package didn't ship /etc/auto.master.d.
    let _ = std::fs::create_dir_all("/etc/auto.master.d");

    // The runtime-scripts phase owns this map and must run first.
    let map_script = "/root/bin/auto.dashusb";
    if !Path::new(map_script).exists() {
        anyhow::bail!(
            "{} not installed yet — runtime scripts phase must run before automount",
            map_script
        );
    }

    std::fs::write(
        "/etc/auto.master.d/dashusb.autofs",
        format!("/tmp/snapshots  {}\n", map_script),
    )?;

    // Legacy image-mount wrapper superseded by autofs.
    let _ = std::fs::remove_file("/root/bin/mount_image.sh");

    emitter.progress("Converting snapshot mountpoints to symlinks");
    convert_snapshot_dirs_to_links(emitter).await;

    emitter.progress("Automount configured.");
    Ok(true)
}

/// Convert existing snapshot mount directories to autofs symlinks.
async fn convert_snapshot_dirs_to_links(emitter: &SetupEmitter) {
    let snapshots_dir = "/backingfiles/snapshots";
    let entries = match std::fs::read_dir(snapshots_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("snap-") {
            continue;
        }
        let snap_path = entry.path();
        let mnt = snap_path.join("mnt");

        if mnt.is_symlink() || !mnt.is_dir() {
            continue;
        }

        // An unmounted directory makes this best-effort unmount fail harmlessly.
        let _ = sentryusb_shell::run("umount", &[&mnt.to_string_lossy()]).await;
        if std::fs::remove_dir(&mnt).is_err() {
            emitter.progress(&format!(
                "Warning: could not remove {} — not converting to symlink",
                mnt.display()
            ));
            continue;
        }

        #[cfg(unix)]
        {
            let target = format!("/tmp/snapshots/{}", name);
            if let Err(e) = std::os::unix::fs::symlink(&target, &mnt) {
                emitter.progress(&format!(
                    "Warning: symlink {} → {} failed: {}",
                    mnt.display(), target, e
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = (name, emitter);
    }
}
