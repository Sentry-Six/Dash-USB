//! Embedded runtime shell scripts, installed to /root/bin/ by
//! [`install_runtime_scripts`].
//!
//! archiveloop, systemd units, and external scripts call these by fixed path
//! and filename, so the names here must not change.

use anyhow::Result;

const REMOUNTFS_RW: &str = r#"#!/bin/bash
mount / -o remount,rw
for _mp in /dashusb /boot/firmware /boot; do
  if findmnt "$_mp" > /dev/null 2>&1; then
    mount "$_mp" -o remount,rw
    break
  fi
done
"#;

const MOUNTOPTSFORIMAGE: &str = r#"#!/bin/bash -eu
source="$1"
read -r offset <<<"$(sfdisk -l -q -o START "$source" | tail -1)"
fstype=$(blkid --probe -o value -s TYPE --offset $((offset*512)) "$source")
offsetopt="offset=$((offset*512))"
timeopt="time_offset=-420"
case $fstype in
  vfat)  echo vfat "utf8,umask=000,$offsetopt,$timeopt" ;;
  exfat) echo exfat "umask=000,$offsetopt,$timeopt" ;;
  *)     echo "$fstype" "$offsetopt,$timeopt" ;;
esac
"#;

const MOUNTIMAGE: &str = r#"#!/bin/bash -eu
source="$1"
mountpoint="$2"
shift 3
opts="$*"
read -r fstype moreopts <<<"$(/root/bin/mountoptsforimage "$source")"
mount -t "$fstype" -o "$opts,$moreopts" "$source" "$mountpoint"
"#;

const MAKE_SNAPSHOT: &str = r#"#!/bin/bash -eu
# Thin wrapper around `dashusb snapshot make` — kept because
# archiveloop and external scripts call this path by filename.
# Forwards "$@" so `make_snapshot.sh nofsck` reaches the Rust binary
# which actually handles the flag (skips the loop-mount + fsck pass).
dashusb snapshot make "$@"
"#;

const RELEASE_SNAPSHOT: &str = r#"#!/bin/bash -eu
dashusb snapshot release "$@"
"#;

const MANAGE_FREE_SPACE: &str = r#"#!/bin/bash -eu
dashusb space manage "$@"
"#;

const FORCE_SYNC: &str = r#"#!/bin/bash -eu
# Force an immediate archive sync by sending SIGUSR1 to archiveloop.
pkill -USR1 -f archiveloop || echo "archiveloop not running"
"#;

const ENABLE_GADGET: &str = r#"#!/bin/bash -eu
dashusb gadget enable "$@"
"#;

const DISABLE_GADGET: &str = r#"#!/bin/bash -eu
dashusb gadget disable "$@"
"#;

/// autofs map script for `/tmp/snapshots`: resolves snap-NNN names to the
/// right disk image and fstype for on-demand read-only mounts.
const AUTO_SENTRYUSB: &str = r#"#!/bin/dash

diskimage="/backingfiles/snapshots/$1/snap.bin"
mountpoint="/backingfiles/snapshots/$1/mnt"
optfile="${diskimage}.opts"

case $1 in
  snap-*)
    ;;
  *)
    exit 1
    ;;
esac

if [ ! -r "$diskimage" ]
then
  /root/bin/release_snapshot.sh "$1"
  exit 1
fi

if [ ! -L "$mountpoint" ] && [ -d "$mountpoint" ]
then
  rmdir "$mountpoint"
  ln -s "/tmp/snapshots/$1" "$mountpoint"
fi

if [ ! -f "$optfile" ]
then
  rm -rf "$optfile"
  /root/bin/mountoptsforimage "${diskimage}" | {
    read -r fstype opts
    echo "-fstype=${fstype},ro,${opts} :${diskimage}" > "$optfile"
  }
fi

cat "$optfile"
"#;


// archiveloop and its supporting scripts, vendored from `run/` at compile
// time. Setup MUST write these out: dashusb-archive.service execs
// /root/bin/archiveloop and nothing else installs it on a clean Pi OS
// (`curl | bash install-pi.sh` does not run pi-gen), so a missing file means
// a crashlooping service and no archive runs.

const ARCHIVELOOP: &str = include_str!("../../../run/archiveloop");
const SEND_LIVE_ACTIVITY: &str = include_str!("../../../run/send-live-activity");
const SEND_PUSH_MESSAGE: &str = include_str!("../../../run/send-push-message");
const TEMPERATURE_MONITOR: &str = include_str!("../../../run/temperature_monitor");
const WAITFORIDLE: &str = include_str!("../../../run/waitforidle");

/// Install all runtime helper scripts to /root/bin/. Announces a phase only
/// when at least one script is missing or has changed, so a re-run after a
/// successful install is a silent no-op.
pub async fn install_runtime_scripts(emitter: &crate::SetupEmitter) -> Result<bool> {
    let _ = std::fs::create_dir_all("/root/bin");

    let scripts: &[(&str, &str)] = &[
        ("remountfs_rw", REMOUNTFS_RW),
        ("mountoptsforimage", MOUNTOPTSFORIMAGE),
        ("mountimage", MOUNTIMAGE),
        ("make_snapshot.sh", MAKE_SNAPSHOT),
        ("release_snapshot.sh", RELEASE_SNAPSHOT),
        ("manage_free_space.sh", MANAGE_FREE_SPACE),
        ("force_sync.sh", FORCE_SYNC),
        ("enable_gadget.sh", ENABLE_GADGET),
        ("disable_gadget.sh", DISABLE_GADGET),
        ("auto.dashusb", AUTO_SENTRYUSB),
        // Archive-flow scripts common to every archive system. The
        // per-system variants (archive-clips.sh, archive-is-reachable.sh,
        // connect-archive.sh, disconnect-archive.sh) each ship their own
        // copy and are installed by `archive::install_archive_scripts`
        // according to ARCHIVE_SYSTEM.
        ("archiveloop", ARCHIVELOOP),
        ("send-live-activity", SEND_LIVE_ACTIVITY),
        ("send-push-message", SEND_PUSH_MESSAGE),
        ("temperature_monitor", TEMPERATURE_MONITOR),
        ("waitforidle", WAITFORIDLE),
    ];

    let all_current = scripts.iter().all(|(name, content)| {
        let path = format!("/root/bin/{}", name);
        std::fs::read_to_string(&path)
            .map(|existing| existing == *content)
            .unwrap_or(false)
    });
    if all_current {
        return Ok(false);
    }

    emitter.begin_phase("runtime_scripts", "Installing runtime scripts");
    emitter.progress("Installing runtime helper scripts...");

    for (name, content) in scripts {
        let path = format!("/root/bin/{}", name);
        std::fs::write(&path, content)?;
        let _ = sentryusb_shell::run("chmod", &["+x", &path]).await;
    }

    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink("/root/bin/mountimage", "/sbin/mount.dashusb");
    }

    emitter.progress("Runtime scripts installed.");
    Ok(true)
}
