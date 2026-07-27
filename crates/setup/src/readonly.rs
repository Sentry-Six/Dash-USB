//! Read-only root filesystem.
//!
//! Makes the SD root filesystem read-only and sets up the tmpfs, bind-mount,
//! and dispatcher scaffolding the rest of the system needs to keep working
//! when it cannot write to /. Getting this wrong bricks networking, DNS,
//! Bluetooth bonds, and fsck on every subsequent boot, so nearly every step
//! here is deliberately best-effort and must not abort the phase.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tracing::info;

use crate::env::SetupEnv;
use crate::SetupEmitter;

const FSTAB_PATH: &str = "/etc/fstab";

/// Make the root filesystem read-only. Returns false when skipped by
/// SKIP_READONLY or already applied.
pub async fn make_readonly(env: &SetupEnv, emitter: &SetupEmitter) -> Result<bool> {
    if env.get_bool("SKIP_READONLY", false) {
        emitter.progress("SKIP_READONLY is set, skipping read-only filesystem setup");
        return Ok(false);
    }

    // Skip if root is already read-only and cmdline.txt already has `ro`.
    if already_readonly(env) {
        return Ok(false);
    }

    emitter.begin_phase("readonly", "Read-only filesystem");
    emitter.progress("Making root filesystem read-only...");

    ensure_boot_rw().await;

    // Disable services that write frequently.
    emitter.progress("Disabling unnecessary services...");
    for svc in &["apt-daily.timer", "apt-daily-upgrade.timer"] {
        let _ = sentryusb_shell::run("systemctl", &["disable", svc]).await;
    }
    // Debian housekeeping timers, pointless on a read-only dashcam appliance:
    // man-db rebuilds a manpage cache nobody reads, dpkg-db-backup snapshots a
    // package DB that never changes post-setup, e2scrub does online ext4
    // scrubbing the boot-time fsck already covers. They burn boot and runtime
    // CPU and I/O competing with the services the car needs. Disabling the
    // timer stops both the periodic runs and the boot-time run.
    for svc in &["man-db.timer", "dpkg-db-backup.timer", "e2scrub_all.timer"] {
        let _ = sentryusb_shell::run("systemctl", &["disable", svc]).await;
    }
    // Conflict with USB gadget / not needed on read-only setups.
    for svc in &["amlogic-adbd", "radxa-adbd", "radxa-usbnet", "armbian-led-state"] {
        let _ = sentryusb_shell::run("systemctl", &["disable", svc]).await;
    }

    // Protect essential packages from autoremove. Non-Raspbian distros (e.g.
    // DietPi) install these as auto-dependencies that `apt-get autoremove
    // --purge` below would sweep away, killing WiFi on the very reboot this
    // phase is preparing for.
    for pkg in &[
        "network-manager", "wpasupplicant", "wpa-supplicant", "ifupdown",
        "dhcpcd", "dhcpcd5", "isc-dhcp-client", "firmware-brcm80211",
        "firmware-realtek", "firmware-atheros", "firmware-iwlwifi",
        "firmware-misc-nonfree",
    ] {
        if sentryusb_shell::run("dpkg", &["-s", pkg]).await.is_ok() {
            let _ = sentryusb_shell::run("apt-mark", &["manual", pkg]).await;
        }
    }

    // Remove packages that write constantly.
    emitter.progress("Removing packages incompatible with read-only root...");
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(180),
        "apt-get",
        &["remove", "-y", "--purge", "triggerhappy", "logrotate", "dphys-swapfile"],
    ).await;
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(180),
        "apt-get",
        &["-y", "autoremove", "--purge"],
    ).await;

    // Replace log management with busybox and install ntp.
    emitter.progress("Installing ntp and busybox-syslogd...");
    let _ = sentryusb_shell::run_with_timeout(
        Duration::from_secs(180),
        "bash",
        &["-c", "apt-get -y install ntp busybox-syslogd; dpkg --purge rsyslog"],
    ).await;

    emitter.progress("Configuring system...");

    // `fastboot` disables fsck on boot. With a read-only root, the boot-time
    // fsck is the ONLY chance to catch corruption before / goes read-only, so
    // fastboot MUST be removed and fsck.mode=auto forced.
    if let Some(cmdline_path) = &env.cmdline_path {
        remove_cmdline_param(cmdline_path, "fastboot")?;
        append_cmdline_param(cmdline_path, "fsck.mode=auto")?;
        append_cmdline_param(cmdline_path, "noswap")?;
        append_cmdline_param(cmdline_path, "ro")?;
    }

    // -c 1: force an fsck on every mount.
    if let Some(root_dev) = &env.root_partition {
        if let Err(e) = sentryusb_shell::run("tune2fs", &["-c", "1", root_dev]).await {
            info!("tune2fs failed for rootfs ({}): {}", root_dev, e);
        }
    }
    if let Err(e) = sentryusb_shell::run(
        "tune2fs", &["-c", "1", "/dev/disk/by-label/mutable"],
    ).await {
        info!("tune2fs failed for mutable: {}", e);
    }

    // Swap is off (noswap above); reclaim the swap file's space.
    let _ = std::fs::remove_file("/var/swap");

    // fake-hwclock must stay functional during setup (configure-rtc.sh may run
    // later and replace it with real hwclock). Without this migration a reboot
    // mid-setup has no time source at all.
    ensure_mutable_mounted(emitter).await;
    let _ = std::fs::create_dir_all("/mutable/etc");

    if !Path::new("/etc/fake-hwclock.data").is_symlink()
        && Path::new("/etc/fake-hwclock.data").exists()
    {
        emitter.progress("Moving fake-hwclock data");
        let _ = std::fs::rename("/etc/fake-hwclock.data", "/mutable/etc/fake-hwclock.data");
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("/mutable/etc/fake-hwclock.data", "/etc/fake-hwclock.data");
    }
    // Delay fake-hwclock until /mutable is mounted.
    if Path::new("/lib/systemd/system/fake-hwclock.service").exists() {
        sed_in_place(
            "/lib/systemd/system/fake-hwclock.service",
            |line| {
                if line.starts_with("Before=") {
                    "After=mutable.mount".to_string()
                } else {
                    line.to_string()
                }
            },
        )?;
    }

    // /var/lib/NetworkManager must end up a tmpfs, not a symlink to /mutable:
    // NM's built-in dnsmasq writes lease files here, and if the path is not
    // writable the AP connection enters an enable/disable loop that thrashes
    // the radio and kills all WiFi.
    if Path::new("/var/lib/NetworkManager").is_dir()
        && !Path::new("/var/lib/NetworkManager").is_symlink()
    {
        emitter.progress("Backing up /var/lib/NetworkManager to mutable");
        let _ = std::fs::create_dir_all("/mutable/var/lib");
        let _ = sentryusb_shell::run(
            "cp", &["-a", "/var/lib/NetworkManager", "/mutable/var/lib/"],
        ).await;
    }
    // Undo any symlink left by a previous broken setup.
    if Path::new("/var/lib/NetworkManager").is_symlink() {
        emitter.progress("Replacing /var/lib/NetworkManager symlink with directory for tmpfs");
        let _ = std::fs::remove_file("/var/lib/NetworkManager");
        let _ = std::fs::create_dir_all("/var/lib/NetworkManager");
    }

    // Connection profiles stay on the root FS so they are available at boot
    // even when /mutable (on USB) has not mounted yet. A copy goes to /mutable
    // for reference and restore.
    if Path::new("/etc/NetworkManager/system-connections").is_dir()
        && !Path::new("/etc/NetworkManager/system-connections").is_symlink()
    {
        emitter.progress("Backing up NetworkManager connection profiles to mutable");
        let _ = std::fs::create_dir_all("/mutable/etc/NetworkManager");
        let _ = sentryusb_shell::run(
            "cp", &["-a", "/etc/NetworkManager/system-connections", "/mutable/etc/NetworkManager/"],
        ).await;
    }
    // Undo a broken symlink, restoring the real directory from /mutable when
    // one is available.
    if Path::new("/etc/NetworkManager/system-connections").is_symlink() {
        emitter.progress("Restoring NetworkManager connection profiles to root FS");
        let _ = std::fs::remove_file("/etc/NetworkManager/system-connections");
        if Path::new("/mutable/etc/NetworkManager/system-connections").is_dir() {
            let _ = sentryusb_shell::run(
                "cp", &["-a", "/mutable/etc/NetworkManager/system-connections", "/etc/NetworkManager/"],
            ).await;
        } else {
            let _ = std::fs::create_dir_all("/etc/NetworkManager/system-connections");
        }
    }

    // BlueZ persists pairing keys to /var/lib/bluetooth. On a read-only root
    // that write fails and bluetooth.service can crash during pairing, so the
    // directory is bind-mounted from `.bluetooth` (dot-prefixed to hide the
    // folder from Finder/Explorer when the drive is plugged into a computer).
    if !Path::new("/mutable/.bluetooth").is_dir() {
        emitter.progress("Creating /mutable/.bluetooth for BlueZ bond persistence");
        let _ = std::fs::create_dir_all("/mutable/.bluetooth");
        if Path::new("/var/lib/bluetooth").is_dir()
            && std::fs::read_dir("/var/lib/bluetooth")
                .map(|mut e| e.next().is_some())
                .unwrap_or(false)
        {
            let _ = sentryusb_shell::run(
                "cp", &["-a", "/var/lib/bluetooth/.", "/mutable/.bluetooth/"],
            ).await;
        }
        let _ = sentryusb_shell::run("chmod", &["700", "/mutable/.bluetooth"]).await;
    }

    // The cloud uploader and notification setup write credential JSON to
    // /root/.dashusb at RUNTIME: cloud pairing and notification config are
    // post-setup user actions, so there is no read-write window during setup.
    // On a read-only root that write fails ("set credentials") and cloud
    // pairing hangs forever on "waiting for browser to finish". Bind-mounting
    // from .dashusb (dot-prefixed to hide the folder from Finder/Explorer)
    // keeps the credentials on the writable /mutable partition.
    if !Path::new("/mutable/.dashusb").is_dir() {
        emitter.progress("Creating /mutable/.dashusb for cloud/notification credential persistence");
        let _ = std::fs::create_dir_all("/mutable/.dashusb");
        if Path::new("/root/.dashusb").is_dir()
            && std::fs::read_dir("/root/.dashusb")
                .map(|mut e| e.next().is_some())
                .unwrap_or(false)
        {
            let _ = sentryusb_shell::run(
                "cp", &["-a", "/root/.dashusb/.", "/mutable/.dashusb/"],
            ).await;
        }
        let _ = sentryusb_shell::run("chmod", &["700", "/mutable/.dashusb"]).await;
    }

    // DHCP lease directories must be real dirs, not symlinks: tmpfs mounts
    // over them below.
    if Path::new("/var/lib/dhcp").is_symlink() {
        emitter.progress("Replacing /var/lib/dhcp symlink with directory for tmpfs");
        let _ = std::fs::remove_file("/var/lib/dhcp");
        let _ = std::fs::create_dir_all("/var/lib/dhcp");
    }
    if Path::new("/var/lib/dhcpcd").is_symlink() {
        emitter.progress("Replacing /var/lib/dhcpcd symlink with directory for tmpfs");
        let _ = std::fs::remove_file("/var/lib/dhcpcd");
        let _ = std::fs::create_dir_all("/var/lib/dhcpcd");
    }

    // /mutable/configs holds user configuration overlays.
    let _ = std::fs::create_dir_all("/mutable/configs");

    // /var/spool moves to tmpfs.
    if Path::new("/var/spool").is_symlink() {
        emitter.progress("fixing /var/spool");
        let _ = std::fs::remove_file("/var/spool");
        let _ = std::fs::create_dir_all("/var/spool");
        let _ = sentryusb_shell::run("chmod", &["755", "/var/spool"]).await;
    } else if Path::new("/var/spool").is_dir() {
        // Wipe existing contents so the tmpfs mount doesn't hide stale data.
        for entry in std::fs::read_dir("/var/spool").into_iter().flatten().flatten() {
            let _ = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                std::fs::remove_dir_all(entry.path())
            } else {
                std::fs::remove_file(entry.path())
            };
        }
    }

    // The tmpfs /var/spool takes its permissions from
    // /usr/lib/tmpfiles.d/var.conf; bump 0755 to 1777 so non-root processes
    // (cron, at) can write.
    if Path::new("/usr/lib/tmpfiles.d/var.conf").exists() {
        sed_in_place("/usr/lib/tmpfiles.d/var.conf", |line| {
            if let Some(idx) = line.find("spool") {
                let (prefix, rest) = line.split_at(idx + "spool".len());
                let trimmed = rest.trim_start();
                if let Some(after_mode) = trimmed.strip_prefix("0755") {
                    return format!("{} 1777{}", prefix, after_mode);
                }
            }
            line.to_string()
        })?;
    }

    // /etc/resolv.conf points at /tmp, a tmpfs that is always writable at
    // boot; a /mutable symlink breaks when the USB drive is slow. This also
    // redirects away from systemd-resolved's stub path, which would conflict:
    // NM is set to dns=none below and a dispatcher populates resolv.conf
    // directly.
    let resolv_target = std::fs::read_link("/etc/resolv.conf")
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if resolv_target != "/tmp/resolv.conf" {
        emitter.progress(&format!(
            "Redirecting resolv.conf to /tmp (was: {})",
            if resolv_target.is_empty() { "empty" } else { &resolv_target }
        ));
        seed_tmp_resolv(&resolv_target).await;
        let _ = std::fs::remove_file("/etc/resolv.conf");
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("/tmp/resolv.conf", "/etc/resolv.conf");
    }

    // tmpfiles.d rule to seed /tmp/resolv.conf on every boot so the symlink
    // doesn't dangle while DHCP/NM come up.
    emitter.progress("Installing tmpfiles.d rule for resolv.conf");
    let _ = std::fs::create_dir_all("/etc/tmpfiles.d");
    std::fs::write(
        "/etc/tmpfiles.d/resolv-fallback.conf",
        "f /tmp/resolv.conf 0644 root root - nameserver 1.1.1.1\n",
    )?;

    // DHCP client hooks that populate /tmp/resolv.conf.
    install_nm_dns_config(emitter).await?;
    install_dhcpcd_hook(emitter).await?;
    install_dhclient_hook(emitter).await?;

    if sentryusb_shell::run("systemctl", &["is-active", "--quiet", "systemd-resolved"])
        .await
        .is_ok()
    {
        emitter.progress("Disabling systemd-resolved (dispatcher handles DNS directly)");
        let _ = sentryusb_shell::run("systemctl", &["stop", "systemd-resolved"]).await;
        let _ = sentryusb_shell::run("systemctl", &["disable", "systemd-resolved"]).await;
    }

    // Unblock now, for the remainder of setup.
    let _ = sentryusb_shell::run("rfkill", &["unblock", "bluetooth"]).await;

    // A oneshot service unblocks BT on every boot. The BT radio starts
    // soft-blocked by default on RPi, and on a read-only root the block never
    // clears, breaking BLE (Tesla BLE key).
    emitter.progress("Installing Bluetooth rfkill-unblock boot service");
    std::fs::write(
        "/etc/systemd/system/rfkill-unblock-bluetooth.service",
        BLUETOOTH_UNBLOCK_SERVICE,
    )?;
    let _ = sentryusb_shell::run(
        "systemctl", &["enable", "rfkill-unblock-bluetooth.service"],
    ).await;

    // Reload the NM config (dns=none plus dispatcher) rather than restarting:
    // a full restart would drop WiFi and kill SSH sessions mid-setup. The
    // reboot that follows applies the new config fully.
    if sentryusb_shell::run("systemctl", &["is-active", "--quiet", "NetworkManager"])
        .await
        .is_ok()
    {
        emitter.progress("Reloading NetworkManager configuration");
        let _ = sentryusb_shell::run("nmcli", &["general", "reload"]).await;
    }

    // fstab: ro on boot and root, tmpfs for the writable paths.
    update_fstab()?;

    // Work around mount warning printed when /etc/fstab is newer than
    // /run/systemd/systemd-units-load.
    let _ = sentryusb_shell::run("touch", &["-t", "197001010000", FSTAB_PATH]).await;

    // autofs depends on network services by default (for NFS mounting). NFS is
    // unused here, and dropping the deps speeds up boot.
    if !Path::new("/etc/systemd/system/autofs.service").exists()
        && Path::new("/lib/systemd/system/autofs.service").exists()
    {
        let orig = std::fs::read_to_string("/lib/systemd/system/autofs.service")
            .unwrap_or_default();
        let filtered: String = orig
            .lines()
            .filter(|l| !l.starts_with("Wants=") && !l.starts_with("After="))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write("/etc/systemd/system/autofs.service", filtered + "\n")?;
    }

    // remountfs_rw stays a bash wrapper for compatibility with existing docs
    // and muscle memory.
    let _ = std::fs::create_dir_all("/root/bin");
    std::fs::write("/root/bin/remountfs_rw", "#!/bin/bash\nmount / -o remount,rw\n")?;
    let _ = sentryusb_shell::run("chmod", &["+x", "/root/bin/remountfs_rw"]).await;

    emitter.progress("Read-only filesystem setup complete.");
    Ok(true)
}

fn already_readonly(env: &SetupEnv) -> bool {
    let existing_fstab = std::fs::read_to_string(FSTAB_PATH).unwrap_or_default();
    let root_ro = existing_fstab.lines().any(|l| {
        !l.starts_with('#') && l.contains(" / ") && l.contains("ext4") && l.contains(",ro")
    });
    let cmdline_ro = env
        .cmdline_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| c.split_whitespace().any(|w| w == "ro"))
        .unwrap_or(false);
    root_ro && cmdline_ro
}

pub async fn ensure_boot_rw() {
    // Order matters: /dashusb is the preferred symlink, /teslausb the legacy
    // name some upgraded installs still have, /boot/firmware the bookworm
    // path. The first mount point that exists wins.
    for mp in &["/dashusb", "/teslausb", "/boot/firmware", "/boot"] {
        if is_mount_point(mp).await {
            let _ = sentryusb_shell::run("mount", &[mp, "-o", "remount,rw"]).await;
            break;
        }
    }
}

async fn is_mount_point(path: &str) -> bool {
    sentryusb_shell::run("findmnt", &[path]).await.is_ok()
}

async fn ensure_mutable_mounted(emitter: &SetupEmitter) {
    if is_mount_point("/mutable").await {
        return;
    }
    let fstab = std::fs::read_to_string(FSTAB_PATH).unwrap_or_default();
    if !fstab.contains("LABEL=mutable") {
        return;
    }
    emitter.progress("Mounting the mutable partition...");
    let _ = sentryusb_shell::run("mount", &["/mutable"]).await;
}

/// Append a parameter to cmdline.txt if it's not already present.
fn append_cmdline_param(path: &str, param: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let trimmed = content.trim();
    if trimmed.split_whitespace().any(|w| w == param) {
        return Ok(());
    }
    std::fs::write(path, format!("{} {}\n", trimmed, param))?;
    info!("Added '{}' to {}", param, path);
    Ok(())
}

/// Remove a parameter from cmdline.txt if present. Preserves the rest of the
/// one-line kernel command line exactly.
fn remove_cmdline_param(path: &str, param: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let trimmed = content.trim();
    let words: Vec<&str> = trimmed.split_whitespace().filter(|w| *w != param).collect();
    let new = words.join(" ");
    if new != trimmed {
        std::fs::write(path, format!("{}\n", new))?;
        info!("Removed '{}' from {}", param, path);
    }
    Ok(())
}

/// Read-modify-write a file line-by-line.
fn sed_in_place<F>(path: &str, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> String,
{
    let content = std::fs::read_to_string(path)?;
    let had_trailing_newline = content.ends_with('\n');
    let new: Vec<String> = content.lines().map(|l| f(l)).collect();
    let mut out = new.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

async fn seed_tmp_resolv(existing_target: &str) {
    // Try nmcli first, then the existing resolv.conf, then fallback to 1.1.1.1.
    let _ = std::fs::write("/tmp/resolv.conf", "");

    if sentryusb_shell::run("nmcli", &["--version"]).await.is_ok() {
        let cmd = "nmcli --terse --fields IP4.DNS dev show 2>/dev/null | \
                   sed -n 's/^IP4\\.DNS\\[.*\\]:/nameserver /p' | head -3 \
                   >> /tmp/resolv.conf";
        let _ = sentryusb_shell::run("bash", &["-c", cmd]).await;
    }

    let has_ns = std::fs::read_to_string("/tmp/resolv.conf")
        .map(|c| c.lines().any(|l| l.starts_with("nameserver")))
        .unwrap_or(false);

    if !has_ns && !existing_target.is_empty() {
        if let Ok(c) = std::fs::read_to_string(existing_target) {
            let ns_lines: String = c
                .lines()
                .filter(|l| l.starts_with("nameserver"))
                .collect::<Vec<_>>()
                .join("\n");
            if !ns_lines.is_empty() {
                let _ = std::fs::write(
                    "/tmp/resolv.conf",
                    format!("{}\n", ns_lines),
                );
            }
        }
    }

    let has_ns = std::fs::read_to_string("/tmp/resolv.conf")
        .map(|c| c.lines().any(|l| l.starts_with("nameserver")))
        .unwrap_or(false);
    if !has_ns {
        let _ = std::fs::write("/tmp/resolv.conf", "nameserver 1.1.1.1\n");
    }
}

async fn install_nm_dns_config(emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("nmcli", &["--version"]).await.is_err() {
        return Ok(());
    }
    emitter.progress("Configuring NetworkManager DNS handling (dns=none + dispatcher)");
    std::fs::create_dir_all("/etc/NetworkManager/conf.d")?;
    std::fs::write(
        "/etc/NetworkManager/conf.d/dashusb-dns.conf",
        "[main]\ndns=none\n",
    )?;

    std::fs::create_dir_all("/etc/NetworkManager/dispatcher.d")?;
    std::fs::write(
        "/etc/NetworkManager/dispatcher.d/50-write-resolv-conf",
        NM_DISPATCHER_SCRIPT,
    )?;
    let _ = sentryusb_shell::run(
        "chmod", &["0755", "/etc/NetworkManager/dispatcher.d/50-write-resolv-conf"],
    ).await;
    Ok(())
}

async fn install_dhcpcd_hook(emitter: &SetupEmitter) -> Result<()> {
    if sentryusb_shell::run("dhcpcd", &["--version"]).await.is_err() {
        return Ok(());
    }
    emitter.progress("Installing dhcpcd hook for resolv.conf");
    std::fs::create_dir_all("/lib/dhcpcd/dhcpcd-hooks")?;
    std::fs::write(
        "/lib/dhcpcd/dhcpcd-hooks/90-dashusb-resolv",
        DHCPCD_HOOK,
    )?;
    let _ = sentryusb_shell::run(
        "chmod", &["0644", "/lib/dhcpcd/dhcpcd-hooks/90-dashusb-resolv"],
    ).await;
    Ok(())
}

async fn install_dhclient_hook(emitter: &SetupEmitter) -> Result<()> {
    // Only for systems using /etc/network/interfaces + dhclient (no NM, no dhcpcd).
    if !Path::new("/etc/network").exists() {
        return Ok(());
    }
    if sentryusb_shell::run("nmcli", &["--version"]).await.is_ok() {
        return Ok(());
    }
    if sentryusb_shell::run("dhcpcd", &["--version"]).await.is_ok() {
        return Ok(());
    }
    emitter.progress("Installing ifupdown hook for resolv.conf");
    std::fs::create_dir_all("/etc/dhcp/dhclient-exit-hooks.d")?;
    std::fs::write(
        "/etc/dhcp/dhclient-exit-hooks.d/dashusb-resolv",
        DHCLIENT_HOOK,
    )?;
    let _ = sentryusb_shell::run(
        "chmod", &["0755", "/etc/dhcp/dhclient-exit-hooks.d/dashusb-resolv"],
    ).await;
    Ok(())
}

fn update_fstab() -> Result<()> {
    let mut fstab = std::fs::read_to_string(FSTAB_PATH).unwrap_or_default();

    // Add `,ro` to the boot and root vfat/ext4 lines when not already present.
    let mut lines: Vec<String> = Vec::new();
    for line in fstab.lines() {
        let commented = line.trim_start().starts_with('#');
        if commented {
            lines.push(line.to_string());
            continue;
        }

        let fields: Vec<&str> = line.split_whitespace().collect();
        let (mp, fstype, opts_idx) = match fields.as_slice() {
            [_, mp, fstype, ..] => (*mp, *fstype, 3usize),
            _ => {
                lines.push(line.to_string());
                continue;
            }
        };

        let add_ro = matches!(
            (mp, fstype),
            ("/boot", "vfat") | ("/boot/firmware", "vfat") | ("/", "ext4")
        );
        if !add_ro {
            lines.push(line.to_string());
            continue;
        }

        let opts = fields.get(opts_idx).copied().unwrap_or("defaults");
        if opts.split(',').any(|o| o == "ro") {
            lines.push(line.to_string());
            continue;
        }

        let mut new_fields: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
        let new_opts = if opts == "defaults" {
            "defaults,ro".to_string()
        } else {
            format!("{},ro", opts)
        };
        if new_fields.len() > opts_idx {
            new_fields[opts_idx] = new_opts;
        }
        lines.push(new_fields.join(" "));
    }
    fstab = lines.join("\n");
    if !fstab.ends_with('\n') {
        fstab.push('\n');
    }

    // Ensure the tmpfs entries exist.
    let tmpfs_entries: &[(&str, &str)] = &[
        ("/var/log", "tmpfs /var/log tmpfs nodev,nosuid 0 0"),
        ("/var/tmp", "tmpfs /var/tmp tmpfs nodev,nosuid 0 0"),
        ("/tmp", "tmpfs /tmp    tmpfs nodev,nosuid 0 0"),
        ("/var/spool", "tmpfs /var/spool tmpfs nodev,nosuid 0 0"),
        ("/var/lib/ntp", "tmpfs /var/lib/ntp tmpfs nodev,nosuid 0 0"),
        // NetworkManager needs mode=0700 so dnsmasq lease files have the
        // right permissions for NM's internal access checks.
        (
            "/var/lib/NetworkManager",
            "tmpfs /var/lib/NetworkManager tmpfs nodev,nosuid,mode=0700 0 0",
        ),
        ("/var/lib/dhcp", "tmpfs /var/lib/dhcp tmpfs nodev,nosuid 0 0"),
        ("/var/lib/dhcpcd", "tmpfs /var/lib/dhcpcd tmpfs nodev,nosuid 0 0"),
        // rfkill state on tmpfs so systemd-rfkill cannot restore the stale
        // soft-block frozen in at the moment root went read-only. Otherwise
        // Bluetooth stays blocked on every boot and BLE (Tesla key) breaks.
        (
            "/var/lib/systemd/rfkill",
            "tmpfs /var/lib/systemd/rfkill tmpfs nodev,nosuid 0 0",
        ),
    ];

    for (mp, entry) in tmpfs_entries {
        if fstab_has_mountpoint(&fstab, mp) {
            continue;
        }
        // The mount point directory must exist; tmpfs mounts over it.
        // /var/lib/ntp is wiped and recreated to guarantee a clean directory
        // at the mount target. `symlink_metadata` does not follow symlinks, so
        // the reset also fires when the path is a symlink to a real directory:
        // mounting a tmpfs over a symlink does not do the right thing.
        if *mp == "/var/lib/ntp" {
            let needs_reset = match std::fs::symlink_metadata(mp) {
                Err(_) => false, // Missing, so create_dir_all below covers it.
                Ok(meta) => meta.file_type().is_symlink() || !meta.is_dir(),
            };
            if needs_reset {
                let _ = std::fs::remove_file(mp);
            }
            let _ = std::fs::create_dir_all(mp);
        } else {
            let _ = std::fs::create_dir_all(mp);
        }
        fstab.push_str(entry);
        fstab.push('\n');
    }

    // Bind-mount /mutable/.bluetooth over /var/lib/bluetooth so BlueZ can
    // persist bond keys on the read-only root FS.
    // x-systemd.requires-mounts-for guarantees /mutable mounts first;
    // x-systemd.before puts the bind in place before bluetoothd starts.
    //
    // Both bind mount points MUST exist before root goes read-only: systemd
    // cannot create a missing mount point on a ro root, and without `nofail`
    // the failed bind takes down local-fs.target and drops the Pi into
    // Emergency Mode on the first boot after setup (#158). mkdir hard-fails
    // here so a boot-breaking fstab entry is never written.
    std::fs::create_dir_all("/var/lib/bluetooth")?;
    if !fstab_has_mountpoint(&fstab, "/var/lib/bluetooth") {
        fstab.push_str(
            "/mutable/.bluetooth /var/lib/bluetooth none \
             bind,nofail,x-systemd.requires-mounts-for=/mutable,x-systemd.before=bluetooth.service 0 0\n",
        );
    }

    // Bind-mount /mutable/.dashusb over /root/.dashusb so the cloud uploader
    // and notification setup can persist credential JSON written at runtime on
    // the read-only root FS. Without it, cloud pairing fails at "set
    // credentials" and hangs. x-systemd.before puts the bind in place before
    // the daemon reads or writes credentials at startup.
    std::fs::create_dir_all("/root/.dashusb")?;
    std::fs::set_permissions(
        "/root/.dashusb",
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    if !fstab_has_mountpoint(&fstab, "/root/.dashusb") {
        fstab.push_str(
            "/mutable/.dashusb /root/.dashusb none \
             bind,nofail,x-systemd.requires-mounts-for=/mutable,x-systemd.before=dashusb.service 0 0\n",
        );
    }

    // Repair bind entries written by v3.13.0 through v3.14.3, which lack
    // `nofail` (a setup re-run after a #158 recovery, or a pre-fix line
    // already present).
    for mp in ["/var/lib/bluetooth", "/root/.dashusb"] {
        add_nofail_to_bind(&mut fstab, mp);
    }

    std::fs::write(FSTAB_PATH, fstab)?;
    Ok(())
}

/// Add `nofail` to the options of an existing bind-mount fstab entry for
/// `mountpoint`, if the entry exists and doesn't already have it.
fn add_nofail_to_bind(fstab: &mut String, mountpoint: &str) {
    let lines: Vec<String> = fstab
        .lines()
        .map(|line| {
            if line.trim_start().starts_with('#') {
                return line.to_string();
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(1) != Some(&mountpoint) || fields.len() < 4 {
                return line.to_string();
            }
            let opts = fields[3];
            if opts.split(',').any(|o| o == "nofail") || !opts.split(',').any(|o| o == "bind") {
                return line.to_string();
            }
            let mut new_fields: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
            new_fields[3] = format!("{},nofail", opts);
            new_fields.join(" ")
        })
        .collect();
    *fstab = lines.join("\n");
    if !fstab.ends_with('\n') {
        fstab.push('\n');
    }
}

fn fstab_has_mountpoint(fstab: &str, mountpoint: &str) -> bool {
    fstab.lines().any(|line| {
        if line.trim_start().starts_with('#') {
            return false;
        }
        let mut fields = line.split_whitespace();
        fields.next(); // Skip the spec field.
        fields.next() == Some(mountpoint)
    })
}

const NM_DISPATCHER_SCRIPT: &str = r#"#!/bin/bash
# Populate /tmp/resolv.conf with DHCP-provided DNS servers.
case "$2" in
  up|dhcp4-change)
    _servers="${DHCP4_DOMAIN_NAME_SERVERS:-${IP4_NAMESERVERS:-}}"
    if [ -n "$_servers" ]; then
      {
        for _ns in $_servers; do
          echo "nameserver $_ns"
        done
        _domain="${DHCP4_DOMAIN_NAME:-}"
        [ -n "$_domain" ] && echo "search $_domain"
      } > /tmp/resolv.conf
    fi
    ;;
esac
"#;

const DHCPCD_HOOK: &str = r#"# Write DHCP-provided DNS servers to /tmp/resolv.conf.
# /etc/resolv.conf is a symlink to /tmp/resolv.conf on DashUSB.
if [ -n "${new_domain_name_servers:-}" ]; then
  {
    for ns in $new_domain_name_servers; do
      echo "nameserver $ns"
    done
    [ -n "${new_domain_name:-}" ] && echo "search $new_domain_name"
  } > /tmp/resolv.conf
fi
"#;

const DHCLIENT_HOOK: &str = r#"# Write DHCP-provided DNS to /tmp/resolv.conf (DashUSB read-only root).
if [ -n "${new_domain_name_servers:-}" ]; then
  {
    for ns in $new_domain_name_servers; do
      echo "nameserver $ns"
    done
    [ -n "${new_domain_name:-}" ] && echo "search $new_domain_name"
  } > /tmp/resolv.conf
fi
"#;

const BLUETOOTH_UNBLOCK_SERVICE: &str = r#"[Unit]
Description=Unblock Bluetooth RF-kill
DefaultDependencies=no
Before=bluetooth.service hciuart.service
After=sysinit.target

[Service]
Type=oneshot
ExecStart=/usr/sbin/rfkill unblock bluetooth

[Install]
WantedBy=multi-user.target
"#;
