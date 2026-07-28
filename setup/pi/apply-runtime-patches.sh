#!/bin/bash
# Re-applies every install-time patch that must survive an OTA update. Called
# by install-pi.sh and by crates/api/src/update.rs after each binary swap.
#
# The in-app updater swaps only the binary, never re-running install-pi.sh, so
# patches made to shipped scripts rot the moment a release replaces those
# scripts. That once left every 4C+ user with a crash-looped Bluetooth stack
# after their first update.
#
# Safe to re-run: each patch checks its own board/precondition and its own
# marker, so it is a no-op where it doesn't apply or has already been applied.

set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[patches]${NC} $1"; }
warn() { echo -e "${YELLOW}[patches]${NC} $1" >&2; }
err()  { echo -e "${RED}[patches]${NC} $1" >&2; }

# ── Detection helpers ────────────────────────────────────────────────────

# Broadcom chips where BlueZ's extended advertising fails or defaults to
# non-connectable parameters, so SC's BLE pair fails without the raw-HCI
# ADV_IND helper. Detected from the chip family ID the kernel logs on first BT
# probe (e.g. "Bluetooth: hci0: BCM43430B0 (002.001.012)").
#
# Affected:
#   BCM4345C0: Rock 4C+ (confirmed in the field)
#   BCM43430B0: Pi Zero 2 W (confirmed by btmon trace 2026-06-20)
#   BCM43438: Pi 3B/3B+, Pi Zero W (same chip family, same firmware tree)
#
# DELIBERATELY EXCLUDED until tested:
#   BCM43455 / CYW43455 (Pi 4 / Pi 5). Their modern bluetoothd path is
#   reported to work, and the raw-HCI helper would override that working
#   ext-adv with legacy adv. A Pi 4/5 user who does hit "GATT 147
#   bond=BOND_NONE" can opt in with:
#       sudo touch /mutable/force-ble-adv-helper
#   That sentinel forces the install regardless of chip detection; the next
#   OTA (or a manual run of this script) lands it.
is_known_broken_ble_chip() {
    # Operator override for chips not yet listed here but field-confirmed to
    # need the helper.
    [ -f /mutable/force-ble-adv-helper ] && { log "BLE adv: /mutable/force-ble-adv-helper present — forcing install"; return 0; }
    local chips="BCM4345C0\|BCM43430B0\|BCM43438"
    dmesg 2>/dev/null | grep -qE "hci0: ($chips)" && return 0
    # dmesg may no longer hold that line on a long-running box, so fall back
    # to the board model. The 4C+'s 4345C0 and the Zero 2 W's 43430B0 are
    # board-specific, so a model match is unambiguous.
    grep -qai 'rock-4c-plus\|rockpi4c-plus\|ROCK 4C+\|Raspberry Pi Zero 2 W\|Raspberry Pi 3 Model B\|Raspberry Pi Zero W' \
        /proc/device-tree/model 2>/dev/null && return 0
    return 1
}

# ── BLE non-fatal-adv patch (all Broadcom Pi-family chips) ──────────────
#
# Broadcom Pi-family chips (BCM4345C0 on Rock 4C+, BCM43430B0 on Pi Zero 2 W,
# the BCM43455 sibling on Pi 4/Compute Module) all reject BlueZ's extended
# advertising with "Invalid Parameters 0x0d". The shipped dashusb-ble.py calls
# sys.exit(1) on that error, tearing down GATT and letting systemd re-spawn
# the daemon in a fast crash loop. Advertising is handled out-of-band by
# dashusb-ble-adv.service over raw HCI (ADV_IND programmed directly), so the
# BlueZ failure is genuinely non-fatal: only the GATT server has to stay up.
# This patch logs the adv error instead of exiting.
apply_ble_nonfatal_adv() {
    local f=/root/bin/dashusb-ble.py
    [ -f "$f" ] || { warn "BLE: $f missing — skipping non-fatal-adv patch"; return 0; }

    if grep -q 'legacy btmgmt advertising' "$f"; then
        log "BLE non-fatal-adv: already patched"
        return 0
    fi

    # Make root RW for the write (no-op if already RW).
    [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true

    # Replace the whole register_ad_error_cb body, located by its def line and
    # the following def.
    local result
    result="$(python3 - "$f" 2>&1 <<'PYEOF'
import sys
p = sys.argv[1]; s = open(p).read()
a = s.find('def register_ad_error_cb(error):'); b = s.find('\ndef register_app_cb', a)
if a >= 0 and b >= 0:
    cb = ("def register_ad_error_cb(error):\n"
          "    # BCM4345C0 (Rock 4C+): BlueZ uses EXTENDED advertising which this chip\n"
          "    # rejects ('Invalid Parameters 0x0d'). Do NOT exit (that tears down GATT\n"
          "    # and loops forever); keep GATT up. Legacy btmgmt advertising is enabled\n"
          "    # out-of-band by dashusb-ble-adv.service.\n"
          "    log.warning(f'BlueZ advertisement registration failed ({error}); '\n"
          "                'using legacy btmgmt advertising instead; GATT stays up.')\n")
    open(p, 'w').write(s[:a] + cb + s[b+1:]); print('patched')
else:
    print('anchor-not-found')
PYEOF
)" || result="python-error"

    if [ "$result" = "patched" ] && grep -q 'legacy btmgmt advertising' "$f"; then
        log "BLE non-fatal-adv: applied via Python patcher"
    else
        warn "BLE non-fatal-adv: Python path failed ($result), trying sed fallback"
        # sed fallback rewrites register_ad_error_cb body line by line
        sed -i '/^def register_ad_error_cb(error):$/,/^def register_app_cb/{
            /^def register_ad_error_cb(error):$/!{
                /^def register_app_cb/!d
            }
        }' "$f"
        sed -i '/^def register_ad_error_cb(error):$/a\    log.warning(f"BlueZ advertisement registration failed ({error}); using legacy btmgmt advertising instead; GATT stays up.")\n' "$f"
        if grep -q 'legacy btmgmt advertising' "$f"; then
            log "BLE non-fatal-adv: applied via sed fallback"
        else
            err  "BLE non-fatal-adv: BOTH patch paths failed — SC discovery may be broken on this install"
            return 1
        fi
    fi

    # Restart so the patch takes effect now rather than at the next reboot.
    # reset-failed clears the crash-loop backoff from the pre-patch state.
    systemctl reset-failed dashusb-ble.service 2>/dev/null || true
    systemctl restart dashusb-ble.service 2>/dev/null || true
    return 0
}

# ── EATT disable (all Pi boards) ────────────────────────────────────────
#
# The BLE GATT here is app-PIN over plain (unencrypted) ATT. Android (14+
# especially) opens EATT (PSM 0x0027) on connect; bluetoothd refuses that
# without an encrypted link and answers with an SMP Security Request, popping
# an OS pair prompt on every connect, or on some phones a silent GATT 147 /
# "Connection lost" tear-down loop with bond=BOND_NONE.
#
# Channels=1 keeps plain ATT (same GATT, same PIN): no prompt, no tear-down,
# and no change to the security model. No board gate, because installs that
# predate the install-time version of this patch have to heal over OTA.
apply_eatt_disable() {
    local conf=/etc/bluetooth/main.conf
    [ -f "$conf" ] || { warn "EATT: $conf missing — skipping"; return 0; }

    if grep -qE '^Channels[[:space:]]*=[[:space:]]*1' "$conf"; then
        log "EATT disable: already applied"
        return 0
    fi

    if grep -qE '^\[GATT\]' "$conf"; then
        if grep -qiE '^[# ]*Channels' "$conf"; then
            sed -i -E 's/^[# ]*Channels[ ]*=.*/Channels = 1/' "$conf"
        else
            sed -i '/^\[GATT\]/a Channels = 1' "$conf"
        fi
    else
        printf '\n[GATT]\nChannels = 1\n' >> "$conf"
    fi

    if grep -qE '^Channels[[:space:]]*=[[:space:]]*1' "$conf"; then
        log "EATT disable: applied to $conf"
        systemctl restart bluetooth 2>/dev/null || true
    else
        err "EATT disable: write to $conf failed (read-only fs? check remountfs_rw)"
        return 1
    fi
    return 0
}

# ── BLE legacy-advertising helper install (all Broadcom Pi-family chips) ──
#
# Fresh installs get these files from install-pi.sh; this brings older installs
# up to parity. Each file is written only when missing or when the on-disk
# copy differs from the current upstream version.
#
# Files installed:
#   /usr/local/bin/dashusb-ble-adv.sh
#   /etc/systemd/system/dashusb-ble-adv.service
#   /etc/udev/rules.d/99-dashusb-ble-hci.rules
#   /etc/systemd/system/dashusb-ble.service.d/wants-bluetooth.conf
apply_ble_adv_helper() {
    # Gate to known-affected chips so Pi 4/5, where bluetoothd's modern
    # ext-adv works, don't get the raw-HCI helper overriding it. See
    # is_known_broken_ble_chip above for the list.
    is_known_broken_ble_chip || { log "BLE adv: chip not in known-broken list — skipping helper install"; return 0; }
    local repo="${REPO:-Sentry-Six/Dash-USB}"
    local base="https://raw.githubusercontent.com/${repo}/main/setup/pi"
    local changed=0

    install_one() {
        # $1 = source filename, $2 = destination path, $3 = mode
        local src="$1" dst="$2" mode="$3"
        local tmp; tmp="$(mktemp)" || { warn "BLE adv: mktemp failed"; return 1; }
        if ! curl -fsSL --max-time 15 "$base/$src" -o "$tmp" 2>/dev/null; then
            rm -f "$tmp"
            warn "BLE adv: failed to fetch $src — leaving any existing copy alone"
            return 1
        fi
        if [ -f "$dst" ] && cmp -s "$tmp" "$dst"; then
            rm -f "$tmp"
            return 0  # already up to date
        fi
        [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true
        install -m "$mode" "$tmp" "$dst"
        rm -f "$tmp"
        changed=1
        log "BLE adv: installed/refreshed $dst"
    }

    install_one dashusb-ble-adv.sh /usr/local/bin/dashusb-ble-adv.sh 755 || return 0
    install_one dashusb-ble-adv.service /etc/systemd/system/dashusb-ble-adv.service 644
    install_one 99-dashusb-ble-hci.rules /etc/udev/rules.d/99-dashusb-ble-hci.rules 644
    mkdir -p /etc/systemd/system/dashusb-ble.service.d
    install_one dashusb-ble-wants-bluetooth.conf \
                /etc/systemd/system/dashusb-ble.service.d/wants-bluetooth.conf 644

    if [ "$changed" = "1" ]; then
        systemctl daemon-reload 2>/dev/null || true
        udevadm control --reload-rules 2>/dev/null || true
        systemctl enable dashusb-ble-adv.service >/dev/null 2>&1 || true
        systemctl restart dashusb-ble-adv.service 2>/dev/null || true
        log "BLE adv: service enabled + restarted"
    else
        log "BLE adv: all files current, nothing to do"
    fi
    return 0
}

# ── bfq scheduler on the backingfiles disk (all boards) ─────────────────
#
# The archive pipeline (rsync reads, snapshot cp) runs under `ionice -c2 -n7`
# so the car's dashcam writes through the USB gadget always win disk access.
# ionice only takes effect under the bfq I/O scheduler; mq-deadline, the Pi OS
# default, ignores I/O priorities. Ship a udev rule so every sd disk gets bfq
# at hotplug/boot, and apply it to the live backingfiles disk when that is safe.
apply_backingfiles_bfq() {
    local rule=/etc/udev/rules.d/60-dashusb-bfq.rules
    local want='ACTION=="add|change", KERNEL=="sd[a-z]", SUBSYSTEM=="block", ATTR{queue/scheduler}="bfq"'

    modprobe bfq 2>/dev/null || true

    if [ ! -f "$rule" ] || [ "$(cat "$rule" 2>/dev/null)" != "$want" ]; then
        [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true
        if printf '%s\n' "$want" > "$rule" 2>/dev/null; then
            udevadm control --reload-rules 2>/dev/null || true
            log "bfq: installed $rule"
        else
            err "bfq: failed to write $rule (read-only fs? check remountfs_rw)"
        fi
    else
        log "bfq: udev rule already current"
    fi

    # Apply to the running system only while the USB gadget is NOT bound.
    # Switching the elevator drains the disk's request queue, which can stall
    # the car's in-flight dashcam writes: exactly the SCSI-timeout drive-drop
    # this patch exists to prevent. This script runs mid-OTA while the car may
    # be recording, so when the gadget is bound, leave it to the udev rule at
    # the next boot.
    if [ -n "$(cat /sys/kernel/config/usb_gadget/dashusb/UDC 2>/dev/null)" ]; then
        log "bfq: gadget is presented to the car — deferring live scheduler switch to next boot (udev rule covers it)"
        return 0
    fi
    # Resolve the disk backing /backingfiles (e.g. /dev/sda2 -> sda) rather
    # than assuming sda.
    local src disk sched
    src="$(findmnt -n -o SOURCE /backingfiles 2>/dev/null)" || true
    [ -n "${src:-}" ] || { log "bfq: /backingfiles not mounted — udev rule will cover next boot"; return 0; }
    disk="$(lsblk -n -o PKNAME "$src" 2>/dev/null | head -1)"
    [ -n "$disk" ] || disk="$(basename "$src" | sed 's/[0-9]*$//')"
    sched="/sys/block/$disk/queue/scheduler"
    if [ -w "$sched" ]; then
        if grep -q '\[bfq\]' "$sched"; then
            log "bfq: already active on $disk"
        elif echo bfq > "$sched" 2>/dev/null; then
            log "bfq: activated on $disk"
        else
            warn "bfq: could not activate on $disk (kernel without bfq?) — ionice will be a no-op"
        fi
    fi
    return 0
}

# ── systemd hardware watchdog (all boards) ──────────────────────────────
#
# journald on these installs is volatile, so a full kernel hang leaves the car
# with a dead drive indefinitely AND destroys the evidence. With the hardware
# watchdog armed, a hung kernel becomes a ~15s reboot and the gadget
# re-presents ~90s later. 15s is within the BCM283x/BCM2712 watchdog hardware
# maximum (~15.9s). This is strictly kernel-hang protection: userspace-only
# wedges don't trip it, because systemd itself pets the watchdog.
apply_hardware_watchdog() {
    local dropin_dir=/etc/systemd/system.conf.d
    local dropin=$dropin_dir/10-dashusb-watchdog.conf
    local want='[Manager]
RuntimeWatchdogSec=15'

    if [ -f "$dropin" ] && [ "$(cat "$dropin" 2>/dev/null)" = "$want" ]; then
        log "watchdog: drop-in already current"
        return 0
    fi
    [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true
    mkdir -p "$dropin_dir" 2>/dev/null || true
    if printf '%s\n' "$want" > "$dropin" 2>/dev/null; then
        # Deliberately no `systemctl daemon-reexec`: this script runs mid-OTA,
        # and re-executing PID 1 (while arming a 15s hardware watchdog) at
        # that moment is risk for no benefit. These boxes reboot at least
        # daily on car power, so the watchdog arms at the next boot.
        log "watchdog: RuntimeWatchdogSec=15 installed (arms at next boot)"
    else
        err "watchdog: failed to write $dropin (read-only fs? check remountfs_rw)"
    fi
    return 0
}

# ── Archive mount lock (CIFS/NFS connect/disconnect scripts) ────────────
#
# The API's backup path and archiveloop coordinate /mnt/archive ownership
# through a shared flock (/tmp/sentryusb_archive_mount.lock; see
# crates/api/src/archive_mount_lock.rs). The lock-aware
# connect/disconnect-archive.sh land on disk only at setup-wizard time
# (crates/setup/src/archive.rs bakes them into the binary), so without this
# refresh an existing CIFS/NFS install keeps running the lock-free scripts and
# the coordination is one-sided.
#
# The heredocs below MUST stay byte-identical to
# run/cifs_archive/{connect,disconnect}-archive.sh (the nfs copies are the
# same files).
apply_archive_mount_lock_scripts() {
    # Only CIFS/NFS archives mount /mnt/archive from fstab. rsync, rclone, and
    # archiveless installs have nothing to lock.
    if ! grep -qE '[[:space:]]/mnt/archive[[:space:]]+(cifs|nfs)[[:space:]]' /etc/fstab 2>/dev/null; then
        log "archive-mount-lock: no CIFS/NFS /mnt/archive fstab entry — not applicable"
        return 0
    fi
    if grep -q 'ARCHIVE_MOUNT_LOCK' /root/bin/connect-archive.sh 2>/dev/null \
       && grep -q 'ARCHIVE_MOUNT_LOCK' /root/bin/disconnect-archive.sh 2>/dev/null; then
        log "archive-mount-lock: already patched"
        return 0
    fi
    [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true

    # Stage, then rename atomically: a power loss or disk-full mid-write must
    # never leave a truncated live script. archiveloop may invoke these at any
    # moment, and a half-written file that already contains the marker would
    # make the next patch run report "already patched".
    cat > /root/bin/connect-archive.sh.new <<'CONNECT_EOF'
#!/bin/bash -eu

# Must match ARCHIVE_MOUNT_LOCK_PATH in crates/api/src/archive_mount_lock.rs
# and disconnect-archive.sh.
ARCHIVE_MOUNT_LOCK=/tmp/sentryusb_archive_mount.lock

# The archive mount is shared with the API's backup path, which may
# mount /mnt/archive itself for a Backup Now and unmount it when done.
# Take the shared flock around the transition so we can't adopt a
# backup-owned mount that's about to be unmounted from under us. The
# API holds the lock for its whole mount+write+unmount (bounded well
# under the wait here). Fail-closed on lock timeout: mounting without
# the lock reopens the adoption race, and archiveloop already handles a
# failed connect by skipping the cycle and retrying next time.
function mount_archive_locked() {
  local mount_point=$1
  [ -z "$mount_point" ] && return 0
  (
    if ! flock -w 300 210
    then
      log "Archive mount lock busy for 300s — failing archive connect (retried next cycle)."
      exit 1
    fi
    ensure_mountpoint_is_mounted_with_retry "$mount_point"
  ) 210>"$ARCHIVE_MOUNT_LOCK"
}

mount_archive_locked "${ARCHIVE_MOUNT:-}"
CONNECT_EOF

    cat > /root/bin/disconnect-archive.sh.new <<'DISCONNECT_EOF'
#!/bin/bash -eu

# Unmount the archive. Without this, the archive mounts can get into a
# state where the archive is reachable via the network, appears to be
# mounted, but the mount is inoperable and any attempt to access it
# results in a "host is down" message.

# Must match ARCHIVE_MOUNT_LOCK_PATH in crates/api/src/archive_mount_lock.rs
# and connect-archive.sh.
ARCHIVE_MOUNT_LOCK=/tmp/sentryusb_archive_mount.lock

unmount_if_set() {
  local mount_point=$1
  if [ -n "$mount_point" ]
  then
    if findmnt --mountpoint "$mount_point" > /dev/null
    then
      if timeout 10 umount -f -l "$mount_point" >> "$LOG_FILE" 2>&1
      then
        log "Unmounted $mount_point."
      else
        log "Failed to unmount $mount_point."
      fi
    else
      log "$mount_point already unmounted."
    fi
  fi
}

# Archive unmount runs in the FOREGROUND under the shared flock, so an
# in-flight API backup (which holds the lock across its mount+write)
# can't have the mount force-lazy-unmounted mid-write. Bounded: the
# umount itself is capped at 10s and the lock wait at 300s, so this
# can't wedge the return to archiveloop the way an uncapped unmount
# once could. Fail-closed on lock timeout: unmounting without the lock
# is exactly the mid-write teardown the lock exists to prevent — skip,
# and the next cycle's disconnect gets another chance.
(
  if ! flock -w 300 210
  then
    log "Archive mount lock busy for 300s — skipping archive unmount this cycle."
    exit 0
  fi
  unmount_if_set "${ARCHIVE_MOUNT:-}"
) 210>"$ARCHIVE_MOUNT_LOCK"
DISCONNECT_EOF

    chmod 755 /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new
    if ! bash -n /root/bin/connect-archive.sh.new || ! bash -n /root/bin/disconnect-archive.sh.new; then
        err "archive-mount-lock: staged scripts failed bash -n — keeping existing scripts"
        rm -f /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new
        return 1
    fi
    # A power loss between these two renames heals on the next run: the marker
    # check at the top requires BOTH files to carry it.
    mv /root/bin/connect-archive.sh.new /root/bin/connect-archive.sh
    mv /root/bin/disconnect-archive.sh.new /root/bin/disconnect-archive.sh
    log "archive-mount-lock: lock-aware connect/disconnect-archive.sh installed"
}

# ── Run all patches ─────────────────────────────────────────────────────

apply_ble_nonfatal_adv
apply_ble_adv_helper
apply_eatt_disable
apply_backingfiles_bfq
apply_hardware_watchdog
apply_archive_mount_lock_scripts

# Append future OTA-surviving patches here. Each must self-check board,
# precondition, and marker so the script stays a no-op where it doesn't apply.
