#!/bin/bash
#
# fake-gm-car.sh — bench harness that impersonates a GM Surround Vision
# Recorder writing to the cam disk image, so the whole capture pipeline
# (snapshot loop → /mutable/Recordings link farm → archiveloop → offsite
# archive) can be soak-tested without a car.
#
# What it does, on a loop:
#   * every SEGMENT_SECS writes one ~SEGMENT_MB file per camera into
#     <mount>/<recording root>/ using real GM names
#     (FRONT_YYYY_MM_DD_T_HH_MM_SS.mp4 …)
#   * deletes files older than ROLLING_MINUTES — the firmware's rolling
#     delete, the exact behavior Dash USB exists to defeat
#
# Run it on the Pi with the USB gadget DISABLED (dashusb gadget disable
# or the web toggle) so the image isn't presented to a host while we
# write it:
#
#   sudo ./fake-gm-car.sh                 # uses /backingfiles/cam_disk.bin
#   sudo ./fake-gm-car.sh /dev/sdX1       # or a real USB stick partition
#
# Overnight pass criteria:
#   1. every filename written here eventually appears under
#      /mutable/Recordings/Continuous/<date>/  (snapshot capture works)
#   2. with an archive target configured, every file lands there exactly
#      once (check /mutable/recordings_archived for the ledger)
#   3. no file is lost across the rolling delete (SNAPSHOT_INTERVAL must
#      stay well under ROLLING_MINUTES)
#   4. pre-fill /backingfiles to squeeze free space and confirm
#      `dashusb space manage` prunes oldest snapshots
#   5. reboot mid-cycle and confirm recovery (boot snapshot, no fsck
#      damage, loop resumes)
#
# For a full end-to-end test including the USB gadget path, run this
# writer on a SECOND Linux machine with the Pi plugged into it as a
# gadget: the mount is then wherever that host mounted the Dash USB
# drive, and the Pi side stays completely untouched.

set -euo pipefail

TARGET="${1:-/backingfiles/cam_disk.bin}"
RECORDING_ROOT="${RECORDING_ROOT:-Android/media/com.gm.ultifi.gmconnectedcameraservice/Recordings/SurroundVisionRecorder}"
SEGMENT_SECS="${SEGMENT_SECS:-300}"
SEGMENT_MB="${SEGMENT_MB:-106}"
ROLLING_MINUTES="${ROLLING_MINUTES:-120}"
CAMERAS=(FRONT LEFT RIGHT REAR)

log() { echo "$(date '+%F %T') fake-gm-car: $*"; }

MNT=""
cleanup() {
    if [ -n "$MNT" ]; then
        umount "$MNT" 2>/dev/null || true
        rmdir "$MNT" 2>/dev/null || true
    fi
}
trap cleanup EXIT

if [ -f "$TARGET" ]; then
    # Disk image: refuse to double-mount under the car's nose.
    if [ -e /sys/kernel/config/usb_gadget/dashusb/UDC ] \
        && [ -s /sys/kernel/config/usb_gadget/dashusb/UDC ]; then
        echo "ERROR: USB gadget is active — disable it first (two writers on one image corrupts the filesystem)" >&2
        exit 1
    fi
    MNT=$(mktemp -d /tmp/fake-gm-car.XXXXXX)
    # Image is partitioned (sfdisk type=c); mount the first partition.
    LOOPDEV=$(losetup -Pf --show "$TARGET")
    mount "${LOOPDEV}p1" "$MNT" || { losetup -d "$LOOPDEV"; exit 1; }
    trap 'umount "$MNT" 2>/dev/null; losetup -d "$LOOPDEV" 2>/dev/null; rmdir "$MNT" 2>/dev/null' EXIT
    DEST="$MNT"
elif [ -d "$TARGET" ]; then
    # Already-mounted directory (second-host gadget test).
    DEST="$TARGET"
else
    # Block device.
    MNT=$(mktemp -d /tmp/fake-gm-car.XXXXXX)
    mount "$TARGET" "$MNT"
    DEST="$MNT"
fi

REC="$DEST/$RECORDING_ROOT"
mkdir -p "$REC"
log "writing to $REC (segment=${SEGMENT_SECS}s x ${SEGMENT_MB}MB x ${#CAMERAS[@]} cams, rolling delete at ${ROLLING_MINUTES}m)"

while true; do
    ts=$(date '+%Y_%m_%d_T_%H_%M_%S')
    for cam in "${CAMERAS[@]}"; do
        # Random data ≈ incompressible, like H.264 — keeps rsync/rclone
        # timing honest. dd from urandom at ~100 MB is a couple seconds
        # on a Pi; fine at a 5-minute cadence.
        dd if=/dev/urandom of="$REC/${cam}_${ts}.mp4.part" bs=1M count="$SEGMENT_MB" status=none
        mv "$REC/${cam}_${ts}.mp4.part" "$REC/${cam}_${ts}.mp4"
    done
    sync
    log "wrote segment $ts"

    # The firmware's rolling delete.
    find "$REC" -name '*.mp4' -mmin +"$ROLLING_MINUTES" -print -delete | while read -r f; do
        log "rolling delete: $(basename "$f")"
    done

    sleep "$SEGMENT_SECS"
done
