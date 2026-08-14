//! Archive backend validation, dependencies, helpers, and service setup.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::env::SetupEnv;
use crate::error::ConfigError;
use crate::SetupEmitter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveSystem {
    Cifs,
    Nfs,
    Rsync,
    Rclone,
    None,
}

impl ArchiveSystem {
    pub fn from_config(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "cifs" => Ok(Self::Cifs),
            "nfs" => Ok(Self::Nfs),
            "rsync" => Ok(Self::Rsync),
            "rclone" => Ok(Self::Rclone),
            "none" | "" => Ok(Self::None),
            other => Err(ConfigError(format!("Unrecognized archive system: {other}")).into()),
        }
    }
}

/// Classify missing keys as configuration errors so auto-resume stops.
fn validate_archive_config(env: &SetupEnv, system: ArchiveSystem) -> Result<()> {
    let require = |key: &str| -> Result<()> {
        if env.config.get(key).map_or(true, |v| v.is_empty()) {
            return Err(
                ConfigError(format!("Required config variable {key} is not set")).into(),
            );
        }
        Ok(())
    };

    match system {
        ArchiveSystem::Rsync => {
            require("RSYNC_USER")?;
            require("RSYNC_SERVER")?;
            require("RSYNC_PATH")?;
        }
        ArchiveSystem::Rclone => {
            require("RCLONE_DRIVE")?;
            require("RCLONE_PATH")?;
        }
        ArchiveSystem::Cifs => {
            require("SHARE_NAME")?;
            require("SHARE_USER")?;
            require("SHARE_PASSWORD")?;
            require("ARCHIVE_SERVER")?;
        }
        ArchiveSystem::Nfs => {
            require("SHARE_NAME")?;
            require("ARCHIVE_SERVER")?;
        }
        ArchiveSystem::None => {}
    }

    Ok(())
}

/// Add deduplicated rsync host keys for non-interactive systemd connections.
async fn trust_rsync_host_key(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    let server = match env.config.get("RSYNC_SERVER") {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(()),
    };

    let _ = std::fs::create_dir_all("/root/.ssh");
    let known_hosts_path = "/root/.ssh/known_hosts";
    let existing = std::fs::read_to_string(known_hosts_path).unwrap_or_default();

    emitter.progress(&format!("Trusting SSH host key for {}...", server));
    let scan = match sentryusb_shell::run_with_timeout(
        Duration::from_secs(15),
        "ssh-keyscan", &["-H", "-T", "5", &server],
    ).await {
        Ok(s) => s,
        Err(e) => {
            // Defer unreachable-server errors to the archive cycle.
            emitter.progress(&format!(
                "ssh-keyscan {} failed: {}. Archiving may need a manual ssh-keyscan later.",
                server, e
            ));
            return Ok(());
        }
    };

    let mut new_lines: Vec<&str> = Vec::new();
    for line in scan.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !existing.lines().any(|e| e.trim() == line) {
            new_lines.push(line);
        }
    }

    if new_lines.is_empty() {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    for l in &new_lines {
        updated.push_str(l);
        updated.push('\n');
    }
    std::fs::write(known_hosts_path, updated)?;
    let _ = sentryusb_shell::run("chmod", &["600", known_hosts_path]).await;
    emitter.progress(&format!(
        "Added {} host key entry/entries for {} to /root/.ssh/known_hosts",
        new_lines.len(), server
    ));
    Ok(())
}

/// Install a tool if its binary is missing. Probes for the binary rather than
/// the dpkg package; see install_required_packages for why.
async fn ensure_tool(binary: &str, package: &str, emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("which", &[binary]).await.is_ok() {
        return Ok(());
    }
    emitter.progress(&format!("Installing {}...", package));
    crate::apt::apt_install(
        |m| emitter.progress(m),
        &[package],
        Duration::from_secs(600),
    ).await.with_context(|| format!("failed to install {}", package))?;
    Ok(())
}

/// Full archive configuration flow. Returns true if the phase did work.
pub async fn configure_archive(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    let archive_system = ArchiveSystem::from_config(&env.get("ARCHIVE_SYSTEM", "none"))?;

    validate_archive_config(env, archive_system)?;

    // Idempotency: rsync present, archive service installed and enabled.
    let rsync_ok = sentryusb_shell::run("which", &["rsync"]).await.is_ok();
    let service_path = std::path::Path::new("/lib/systemd/system/dashusb-archive.service");
    let service_enabled = sentryusb_shell::run(
        "systemctl", &["is-enabled", "dashusb-archive.service"],
    ).await.is_ok();

    if rsync_ok && service_path.exists() && service_enabled && archive_system == ArchiveSystem::None {
        return Ok(false);
    }

    emitter.begin_phase("archive", "Archive configuration");
    emitter.progress(&format!("Configuring archive system: {:?}", archive_system));

    ensure_tool("rsync", "rsync", emitter).await?;
    // rclone needs its own client rather than a mount helper.
    if archive_system == ArchiveSystem::Rclone {
        ensure_tool("rclone", "rclone", emitter).await?;
    }

    // Mount backends require fstab because connect-archive mounts by path.
    // `noauto` prevents an unavailable NAS from delaying boot.
    match archive_system {
        ArchiveSystem::Nfs => configure_nfs_mount(env, emitter).await?,
        ArchiveSystem::Cifs => configure_cifs_mount(env, emitter).await?,
        ArchiveSystem::Rsync => trust_rsync_host_key(env, emitter).await?,
        _ => {}
    }

    // archiveloop calls backend-specific helpers through fixed filenames.
    install_archive_scripts(archive_system, emitter)?;

    crate::system::install_archive_service()?;
    let _ = sentryusb_shell::run("systemctl", &["daemon-reload"]).await;
    let _ = sentryusb_shell::run("systemctl", &["enable", "dashusb-archive.service"]).await;

    emitter.progress("Archive configuration complete.");
    Ok(true)
}

// Install one backend's complete fixed-name helper set so changing backends
// cannot retain a helper from the previous selection.

const CIFS_ARCHIVE_CLIPS: &str = include_str!("../../../run/cifs_archive/archive-clips.sh");
const CIFS_ARCHIVE_IS_REACHABLE: &str = include_str!("../../../run/cifs_archive/archive-is-reachable.sh");
const CIFS_CONNECT_ARCHIVE: &str = include_str!("../../../run/cifs_archive/connect-archive.sh");
const CIFS_DISCONNECT_ARCHIVE: &str = include_str!("../../../run/cifs_archive/disconnect-archive.sh");

const NFS_ARCHIVE_CLIPS: &str = include_str!("../../../run/nfs_archive/archive-clips.sh");
const NFS_ARCHIVE_IS_REACHABLE: &str = include_str!("../../../run/nfs_archive/archive-is-reachable.sh");
const NFS_CONNECT_ARCHIVE: &str = include_str!("../../../run/nfs_archive/connect-archive.sh");
const NFS_DISCONNECT_ARCHIVE: &str = include_str!("../../../run/nfs_archive/disconnect-archive.sh");

const RSYNC_ARCHIVE_CLIPS: &str = include_str!("../../../run/rsync_archive/archive-clips.sh");
const RSYNC_ARCHIVE_IS_REACHABLE: &str = include_str!("../../../run/rsync_archive/archive-is-reachable.sh");
const RSYNC_CONNECT_ARCHIVE: &str = include_str!("../../../run/rsync_archive/connect-archive.sh");
const RSYNC_DISCONNECT_ARCHIVE: &str = include_str!("../../../run/rsync_archive/disconnect-archive.sh");

const RCLONE_ARCHIVE_CLIPS: &str = include_str!("../../../run/rclone_archive/archive-clips.sh");
const RCLONE_ARCHIVE_IS_REACHABLE: &str = include_str!("../../../run/rclone_archive/archive-is-reachable.sh");
const RCLONE_CONNECT_ARCHIVE: &str = include_str!("../../../run/rclone_archive/connect-archive.sh");
const RCLONE_DISCONNECT_ARCHIVE: &str = include_str!("../../../run/rclone_archive/disconnect-archive.sh");

const NONE_ARCHIVE_CLIPS: &str = include_str!("../../../run/none_archive/archive-clips.sh");
const NONE_ARCHIVE_IS_REACHABLE: &str = include_str!("../../../run/none_archive/archive-is-reachable.sh");
const NONE_CONNECT_ARCHIVE: &str = include_str!("../../../run/none_archive/connect-archive.sh");
const NONE_DISCONNECT_ARCHIVE: &str = include_str!("../../../run/none_archive/disconnect-archive.sh");

/// Install the selected backend's helpers in `/root/bin` with mode 0755.
fn install_archive_scripts(system: ArchiveSystem, emitter: &SetupEmitter) -> Result<()> {
    let _ = std::fs::create_dir_all("/root/bin");

    let scripts: &[(&str, &str)] = match system {
        ArchiveSystem::Cifs => &[
            ("archive-clips.sh", CIFS_ARCHIVE_CLIPS),
            ("archive-is-reachable.sh", CIFS_ARCHIVE_IS_REACHABLE),
            ("connect-archive.sh", CIFS_CONNECT_ARCHIVE),
            ("disconnect-archive.sh", CIFS_DISCONNECT_ARCHIVE),
        ],
        ArchiveSystem::Nfs => &[
            ("archive-clips.sh", NFS_ARCHIVE_CLIPS),
            ("archive-is-reachable.sh", NFS_ARCHIVE_IS_REACHABLE),
            ("connect-archive.sh", NFS_CONNECT_ARCHIVE),
            ("disconnect-archive.sh", NFS_DISCONNECT_ARCHIVE),
        ],
        ArchiveSystem::Rsync => &[
            ("archive-clips.sh", RSYNC_ARCHIVE_CLIPS),
            ("archive-is-reachable.sh", RSYNC_ARCHIVE_IS_REACHABLE),
            ("connect-archive.sh", RSYNC_CONNECT_ARCHIVE),
            ("disconnect-archive.sh", RSYNC_DISCONNECT_ARCHIVE),
        ],
        ArchiveSystem::Rclone => &[
            ("archive-clips.sh", RCLONE_ARCHIVE_CLIPS),
            ("archive-is-reachable.sh", RCLONE_ARCHIVE_IS_REACHABLE),
            ("connect-archive.sh", RCLONE_CONNECT_ARCHIVE),
            ("disconnect-archive.sh", RCLONE_DISCONNECT_ARCHIVE),
        ],
        ArchiveSystem::None => &[
            ("archive-clips.sh", NONE_ARCHIVE_CLIPS),
            ("archive-is-reachable.sh", NONE_ARCHIVE_IS_REACHABLE),
            ("connect-archive.sh", NONE_CONNECT_ARCHIVE),
            ("disconnect-archive.sh", NONE_DISCONNECT_ARCHIVE),
        ],
    };

    for (name, content) in scripts {
        let path = format!("/root/bin/{}", name);
        std::fs::write(&path, *content)
            .with_context(|| format!("write {}", path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
    }

    emitter.progress(&format!("Installed {} archive helper scripts", scripts.len()));
    Ok(())
}

/// Install the named package unless dpkg already reports it. Used by the
/// on-demand archive-helper installs.
async fn ensure_pkg(pkg: &str, emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("dpkg", &["-s", pkg]).await.is_ok() {
        return Ok(());
    }
    emitter.progress(&format!("Installing {}...", pkg));
    sentryusb_shell::run_with_timeout(
        Duration::from_secs(240),
        "apt-get",
        &[
            "-o", "DPkg::Lock::Timeout=180",
            "install", "-y", "--no-install-recommends", pkg,
        ],
    )
    .await
    .with_context(|| format!("failed to install {}", pkg))?;
    Ok(())
}

/// Replace the exact mount-point/filesystem entry while preserving other fstab entries.
fn replace_fstab_entry(fstype: &str, mount_point: &str, new_line: &str) -> Result<()> {
    // Standalone archive setup may not have remounted root read-write.
    let _ = std::process::Command::new("mount")
        .args(["/", "-o", "remount,rw"])
        .output();

    let existing = std::fs::read_to_string("/etc/fstab").unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            // Match complete fields to avoid removing substring lookalikes.
            let fields: Vec<&str> = l.split_whitespace().collect();
            !(fields.len() >= 3 && fields[1] == mount_point && fields[2] == fstype)
        })
        .map(|s| s.to_string())
        .collect();
    lines.push(new_line.to_string());
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write("/etc/fstab", out).context("write /etc/fstab")?;
    Ok(())
}

async fn configure_nfs_mount(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    let server = env.get("ARCHIVE_SERVER", "");
    let share = env.get("SHARE_NAME", "");
    if server.is_empty() || share.is_empty() {
        return Ok(());
    }

    ensure_pkg("nfs-common", emitter).await?;
    std::fs::create_dir_all("/mnt/archive").context("mkdir /mnt/archive")?;

    // vers=3 plus proto=tcp has broader NAS compatibility (UniFi Drive,
    // Synology DSM 7, TrueNAS) than defaulting to v4.2, and `nolock` avoids an
    // unnecessary NLM lock-server dependency.
    let line = format!(
        "{}:{} /mnt/archive nfs rw,noauto,nolock,proto=tcp,vers=3 0 0",
        server, share
    );
    replace_fstab_entry("nfs", "/mnt/archive", &line)?;
    emitter.progress("Added NFS mount to /etc/fstab");

    Ok(())
}

async fn configure_cifs_mount(env: &SetupEnv, emitter: &SetupEmitter) -> Result<()> {
    let server = env.get("ARCHIVE_SERVER", "");
    let share = env.get("SHARE_NAME", "");
    let user = env.get("SHARE_USER", "");
    let pass = env.get("SHARE_PASSWORD", "");
    let domain = env.get("SHARE_DOMAIN", "");
    let vers = env.get("CIFS_VERSION", "3.0");
    if server.is_empty() || share.is_empty() || user.is_empty() || pass.is_empty() {
        return Ok(());
    }

    ensure_pkg("cifs-utils", emitter).await?;

    // Keep credentials out of world-readable fstab.
    let creds_path = "/root/.dashusbArchiveCredentials";
    let mut creds = format!("username={}\npassword={}\n", user, pass);
    if !domain.is_empty() {
        creds.push_str(&format!("domain={}\n", domain));
    }
    std::fs::write(creds_path, creds).context("write credentials file")?;
    // Use chmod so this crate still checks on non-Unix development hosts.
    let _ = sentryusb_shell::run("chmod", &["600", creds_path]).await;

    std::fs::create_dir_all("/mnt/archive").context("mkdir /mnt/archive")?;

    // fstab encodes spaces in paths as \040, which preserves share names like
    // "Dash Cam" without breaking the field split.
    let share_escaped = share.replace(' ', "\\040");
    let line = format!(
        "//{}/{} /mnt/archive cifs rw,noauto,credentials={},iocharset=utf8,file_mode=0777,dir_mode=0777,vers={} 0 0",
        server, share_escaped, creds_path, vers
    );
    replace_fstab_entry("cifs", "/mnt/archive", &line)?;
    emitter.progress("Added CIFS mount to /etc/fstab");

    Ok(())
}
