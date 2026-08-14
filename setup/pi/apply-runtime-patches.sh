#!/bin/bash
# Reapply idempotent, hardware-gated install patches after binary-only updates.

set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[patches]${NC} $1"; }
warn() { echo -e "${YELLOW}[patches]${NC} $1" >&2; }
err()  { echo -e "${RED}[patches]${NC} $1" >&2; }

# ── Detection helpers ────────────────────────────────────────────────────

is_rock_4cplus() {
    grep -qai 'rock-4c-plus\|rockpi4c-plus\|ROCK 4C+' \
        /proc/device-tree/model /proc/device-tree/compatible 2>/dev/null
}

# Broadcom chips requiring raw-HCI ADV_IND because BlueZ extended advertising
# fails or becomes non-connectable.
#
# Affected: BCM4345C0 (Rock 4C+), BCM43430B0 (Pi Zero 2 W), and BCM43438
# (Pi 3B/3B+ and Pi Zero W).
#
# Excluded: BCM43455/CYW43455 (Pi 4/5), where modern advertising works. An
# operator can force the helper with:
#       sudo touch /mutable/force-ble-adv-helper
is_known_broken_ble_chip() {
    # Operator override for an unlisted affected chip.
    [ -f /mutable/force-ble-adv-helper ] && { log "BLE adv: /mutable/force-ble-adv-helper present — forcing install"; return 0; }
    local chips="BCM4345C0\|BCM43430B0\|BCM43438"
    dmesg 2>/dev/null | grep -qE "hci0: ($chips)" && return 0
    # Fall back to models whose Bluetooth chips are unambiguous.
    grep -qai 'rock-4c-plus\|rockpi4c-plus\|ROCK 4C+\|Raspberry Pi Zero 2 W\|Raspberry Pi 3 Model B\|Raspberry Pi Zero W' \
        /proc/device-tree/model 2>/dev/null && return 0
    return 1
}

# ── BLE non-fatal-adv patch (all Broadcom Pi-family chips) ──────────────
#
# Keep GATT alive when Broadcom rejects BlueZ extended advertising; the raw-HCI
# helper handles advertising separately.
apply_ble_nonfatal_adv() {
    local f=/root/bin/dashusb-ble.py
    [ -f "$f" ] || { warn "BLE: $f missing — skipping non-fatal-adv patch"; return 0; }

    if grep -q 'legacy btmgmt advertising' "$f"; then
        log "BLE non-fatal-adv: already patched"
        return 0
    fi

    [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true

    # Replace the callback body between function definitions.
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
        # Fallback rewrites the callback body line by line.
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

    # Clear any pre-patch crash-loop backoff before restarting.
    systemctl reset-failed dashusb-ble.service 2>/dev/null || true
    systemctl restart dashusb-ble.service 2>/dev/null || true
    return 0
}

# ── EATT disable (all Pi boards) ────────────────────────────────────────
#
# Android may open encrypted EATT (PSM 0x0027), but this service authenticates
# over plain ATT with an app PIN. Channels=1 prevents OS pairing prompts and
# GATT teardown loops.
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

# ── BLE legacy-advertising helper install (known-broken Broadcom chips) ──
#
# Files installed:
#   /usr/local/bin/dashusb-ble-adv.sh
#   /etc/systemd/system/dashusb-ble-adv.service
#   /etc/udev/rules.d/99-dashusb-ble-hci.rules
#   /etc/systemd/system/dashusb-ble.service.d/wants-bluetooth.conf
apply_ble_adv_helper() {
    # Do not replace working Pi 4/5 extended advertising.
    is_known_broken_ble_chip || { log "BLE adv: chip not in known-broken list — skipping helper install"; return 0; }
    # Rust callers provide validated source coordinates; manual runs use defaults.
    local repo="${DASHUSB_REPO_SLUG:-Sentry-Six/Dash-USB}"
    local ref="${DASHUSB_REF:-main}"
    local base="https://raw.githubusercontent.com/${repo}/${ref}/setup/pi"
    local changed=0

    install_one() {
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
# Archive I/O runs under `ionice -c2 -n7`, which requires BFQ to prioritize car
# writes. Install a boot/hotplug rule and switch the live backing disk only when
# the gadget is unbound.
apply_backingfiles_bfq() {
    local rc=0
    local rule=/etc/udev/rules.d/60-dashusb-bfq.rules
    local want='ACTION=="add|change", KERNEL=="sd[a-z]|mmcblk[0-9]", SUBSYSTEM=="block", ATTR{queue/scheduler}="bfq"'

    modprobe bfq 2>/dev/null || true

    if [ ! -f "$rule" ] || [ "$(cat "$rule" 2>/dev/null)" != "$want" ]; then
        [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true
        if printf '%s\n' "$want" > "$rule" 2>/dev/null; then
            udevadm control --reload-rules 2>/dev/null || true
            log "bfq: installed $rule"
        else
            err "bfq: failed to write $rule (read-only fs? check remountfs_rw)"
            # A missing rule would silently revert the policy at next boot.
            rc=1
        fi
    else
        log "bfq: udev rule already current"
    fi

    # Switching the scheduler drains the request queue. Defer while the gadget
    # is bound so in-flight dashcam writes are not stalled.
    if [ -n "$(cat /sys/kernel/config/usb_gadget/dashusb/UDC 2>/dev/null)" ]; then
        log "bfq: gadget is presented to the car — deferring live scheduler switch to next boot (udev rule covers it)"
        return $rc
    fi
    # Resolve the disk backing /backingfiles (e.g. /dev/sda2 -> sda) rather
    # than assuming sda.
    local src disk sched
    src="$(findmnt -n -o SOURCE /backingfiles 2>/dev/null)" || true
    [ -n "${src:-}" ] || { log "bfq: /backingfiles not mounted — udev rule will cover next boot"; return $rc; }
    disk="$(lsblk -n -o PKNAME "$src" 2>/dev/null | head -1)"
    # mmcblk/nvme partition names use <disk>p<N>; other disks use <disk><N>.
    if [ -z "$disk" ]; then
        disk="$(basename "$src")"
        case "$disk" in
            mmcblk*|nvme*) disk="$(echo "$disk" | sed 's/p[0-9]*$//')" ;;
            *)             disk="$(echo "$disk" | sed 's/[0-9]*$//')" ;;
        esac
    fi
    # Match the udev rule's device-class gate so live and boot policy agree.
    case "$disk" in
        sd[a-z]|mmcblk[0-9]) ;;
        *)
            log "bfq: $disk is not sd*/mmcblk* (NVMe?) — excluded by design, leaving its scheduler alone"
            return $rc
            ;;
    esac
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
    # Live-switch failure is advisory because the udev rule applies at boot.
    return $rc
}

# ── systemd hardware watchdog (all boards) ──────────────────────────────
#
# The 15-second hardware watchdog recovers kernel hangs; systemd services must
# keep running to pet it. The timeout is within BCM283x/BCM2712 limits.
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
        # Do not re-exec PID 1 during OTA; the watchdog arms at next boot.
        log "watchdog: RuntimeWatchdogSec=15 installed (arms at next boot)"
    else
        err "watchdog: failed to write $dropin (read-only fs? check remountfs_rw)"
    fi
    return 0
}

# ── Archive mount lock (CIFS/NFS connect/disconnect scripts) ────────────
#
# Refresh existing CIFS/NFS installs with the shared archive-mount flock used
# by API backups and archiveloop.
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

    # Stage before renaming so a failed write cannot truncate a live script.
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

    if ! chmod 755 /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new; then
        err "archive-mount-lock: chmod on staged scripts failed — keeping existing scripts"
        rm -f /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new
        return 1
    fi
    if ! bash -n /root/bin/connect-archive.sh.new || ! bash -n /root/bin/disconnect-archive.sh.new; then
        err "archive-mount-lock: staged scripts failed bash -n — keeping existing scripts"
        rm -f /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new
        return 1
    fi
    # The marker check requires both scripts, so a partial install is retried.
    if ! mv /root/bin/connect-archive.sh.new /root/bin/connect-archive.sh; then
        err "archive-mount-lock: installing connect-archive.sh failed — keeping existing scripts"
        rm -f /root/bin/connect-archive.sh.new /root/bin/disconnect-archive.sh.new
        return 1
    fi
    if ! mv /root/bin/disconnect-archive.sh.new /root/bin/disconnect-archive.sh; then
        err "archive-mount-lock: connect-archive.sh was replaced but disconnect-archive.sh was NOT — one-sided install, re-run this script"
        rm -f /root/bin/disconnect-archive.sh.new
        return 1
    fi
    log "archive-mount-lock: lock-aware connect/disconnect-archive.sh installed"
    return 0
}

# ── Rock 4C+ WiFi NVRAM: remove the TX-collapsing AP6256 relink ─────────
# The AP6256 NVRAM relink collapses TX throughput on Rock 4C+. Remove only that
# exact link so the driver uses generic brcmfmac43455-sdio.txt after reboot.
apply_4cplus_wifi_nvram_fix() {
    is_rock_4cplus || return 0
    local brcm=/lib/firmware/brcm
    local link="$brcm/brcmfmac43455-sdio.radxa,rock-4c-plus.txt"
    # Preserve any board link that does not target nvram_ap6256.txt.
    [ -L "$link" ] || { log "4c+ wifi nvram: no board relink — generic in use"; return 0; }
    if [ "$(basename "$(readlink "$link")")" != "nvram_ap6256.txt" ]; then
        log "4c+ wifi nvram: board .txt not the AP6256 relink — leaving as-is"
        return 0
    fi
    # Restore the root mount state after removing the link.
    local ro_before=no rc=0
    findmnt -no OPTIONS / 2>/dev/null | grep -qE '(^|,)ro(,|$)' && ro_before=yes
    [ -x /root/bin/remountfs_rw ] && /root/bin/remountfs_rw >/dev/null 2>&1 || true
    if rm -f "$link" 2>/dev/null; then
        log "4c+ wifi nvram: removed AP6256 relink → generic fallback (REBOOT to apply)"
    else
        err "4c+ wifi nvram: could not remove $link (read-only fs? check remountfs_rw)"
        rc=1
    fi
    if [ "$ro_before" = yes ]; then
        sync
        mount -o remount,ro / 2>/dev/null || true
    fi
    # Preserve the removal result across the remount.
    return $rc
}

# ── Run all patches ─────────────────────────────────────────────────────

# Run every patch, then report failure if any patch failed.
PATCH_FAILURES=0
run_patch() {
    local fn="$1"
    if ! "$fn"; then
        PATCH_FAILURES=$((PATCH_FAILURES + 1))
        err "$fn: FAILED"
    fi
}

run_patch apply_ble_nonfatal_adv
run_patch apply_ble_adv_helper
run_patch apply_eatt_disable
run_patch apply_backingfiles_bfq
run_patch apply_hardware_watchdog
run_patch apply_archive_mount_lock_scripts
run_patch apply_4cplus_wifi_nvram_fix

# Every patch must check its board, preconditions, and idempotence marker.

if [ "$PATCH_FAILURES" -gt 0 ]; then
    err "$PATCH_FAILURES patch(es) failed to apply — see messages above"
    exit 1
fi
exit 0
