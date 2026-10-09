#!/bin/bash
# Run the shipped functions against temporary data, replacing only hardware
# and networking boundaries. Requires bash, flock, setsid and GNU sync.
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
unrelated=''
cleanup() {
  [ -z "$unrelated" ] || kill "$unrelated" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
export ARCHIVE_CONTROL_DIR="$work"
source run/archive-control.sh
extract() {
  awk -v name="$1" '$0 ~ "^function " name "([ (]|$)" {keep=1} keep {print} keep && /^}/ {exit}' run/archiveloop
}
for name in run_archive_cycle cancel_archive_cleanup archive_clips clean_cam_mount clean_cam_mount_locked wait_for_archive_to_be_unreachable; do
  eval "$(extract "$name" | sed "s@/root/bin/@$work/bin/@g; s@/mutable/@$work/mutable/@g; s@/tmp/archive_is_unreachable@$work/unreachable@g")"
done
mkdir -p "$work/bin" "$work/mutable" "$work/cam"
export LOG_FILE="$work/log"
export CAM_CLEANUP_DEFERRED="$work/mutable/deferred"
export CAM_MOUNT="$work/cam"
log() { echo "$*" >> "$LOG_FILE"; }
for cmd in connect-archive.sh disconnect-archive.sh send-live-activity; do
  printf '#!/bin/bash\nprintf "%%s\\n" "%s $*" >> "%s"\n' "$cmd" "$work/calls" > "$work/bin/$cmd"
  chmod +x "$work/bin/$cmd"
done
travel_mode_active() { return 1; }
archive_is_reachable() { return 0; }
has_cam_disk() { return 0; }
clear_archive_status() { echo clear >> "$work/calls"; }
ensure_usb_drives_connected() { echo restore >> "$work/calls"; }

# A transfer's whole process group stops; an unrelated job survives.
archive_cycle_begin
first=$ARCHIVE_CYCLE_ID
sleep 60 & unrelated=$!
(sleep 0.2; touch "$work/archive-cycle-cancel-$first") & request=$!
rc=0
archive_run_command bash -c 'echo "$BASHPID" > "$1"; sleep 60 & wait' _ "$work/child" || rc=$?
wait "$request"
[ "$rc" = 125 ]
kill -0 "$unrelated"
! kill -0 "$(cat "$work/child")" 2>/dev/null
archive_cycle_end
[ "$ARCHIVE_CYCLE_CANCELLED" = 1 ]

# The old request cannot poison a newly generated cycle.
archive_cycle_begin
[ "$first" != "$ARCHIVE_CYCLE_ID" ]
archive_run_command bash -c 'echo done' > "$work/result"
[ "$(cat "$work/result")" = "done" ]
archive_cycle_end

# Cancellation during transfer preserves footage, clears transient progress,
# restores USB, unmounts the archive and suppresses the success outcome.
archive_recordings() {
  touch "$work/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
  return 125
}
echo footage > "$CAM_MOUNT/short.mp4"
echo progress > "$work/mutable/archive_in_progress.json"
: > "$work/calls"
run_archive_cycle
[ "$ARCHIVE_CYCLE_CANCELLED" = 1 ]
[ "$ARCHIVE_CYCLE_FAILED" = 0 ]
[ -f "$CAM_CLEANUP_DEFERRED" ]
[ ! -e "$work/mutable/archive_in_progress.json" ]
[ "$(grep -c '^restore$' "$work/calls")" = 1 ]
grep -q 'send-live-activity end cancelled' "$work/calls"
! grep -q 'end complete' "$work/calls"
[ ! -e "$work/archive-cycle" ]
ensure_cam_file_is_mounted() { echo 'cancelled cleanup mounted the live image' >&2; exit 1; }
clean_cam_mount freespace
clean_cam_mount boot
[ "$(cat "$CAM_MOUNT/short.mp4")" = footage ]

# Cancellation while connecting must also restore recording and stop the cycle.
cat > "$work/bin/connect-archive.sh" <<'SCRIPT'
#!/bin/bash
touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
sleep 60
SCRIPT
: > "$work/calls"
run_archive_cycle
[ "$ARCHIVE_CYCLE_CANCELLED" = 1 ]
[ "$(grep -c '^restore$' "$work/calls")" = 1 ]
printf '#!/bin/bash\nexit 0\n' > "$work/bin/connect-archive.sh"

# A late accepted request during final teardown must still run cleanup once.
rm "$CAM_CLEANUP_DEFERRED"
archive_recordings() { return 0; }
cat > "$work/bin/disconnect-archive.sh" <<'SCRIPT'
#!/bin/bash
if [ -n "${ARCHIVE_CYCLE_ID:-}" ]; then
  touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
fi
SCRIPT
: > "$work/calls"
run_archive_cycle
[ "$ARCHIVE_CYCLE_CANCELLED" = 1 ]
[ "$ARCHIVE_CANCEL_CLEANED" = 1 ]
[ -f "$CAM_CLEANUP_DEFERRED" ]
# Normal restoration plus the late-cancel ensure; active gadgets do not cycle.
[ "$(grep -c '^restore$' "$work/calls")" = 2 ]

# A request accepted during cleanup-deferral release must retain protection.
archive_cycle_begin
touch "$work/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
rc=0
archive_allow_cleanup_after_success || rc=$?
[ "$rc" = 125 ]
[ -f "$CAM_CLEANUP_DEFERRED" ]
archive_cycle_end

# Travel Mode must not instantly resume a cancelled archive on the same link.
travel_mode_active() { return 0; }
retry() { echo probed >> "$work/probes"; return 1; }
ARCHIVE_CYCLE_CANCELLED=1
wait_for_archive_to_be_unreachable
[ -s "$work/probes" ]
# Normal Travel Mode still switches to interval pacing without a probe.
rm "$work/probes"
ARCHIVE_CYCLE_CANCELLED=0
wait_for_archive_to_be_unreachable
[ ! -e "$work/probes" ]

# A later successful visit can release durable deferral and end normally.
archive_cycle_begin
archive_allow_cleanup_after_success
[ ! -e "$CAM_CLEANUP_DEFERRED" ]
archive_cycle_end

echo 'archive cancellation: transfer, durable deferral, late request, USB restoration and next cycle passed'
