#!/bin/bash -eu

# read the setup variables again because arrays, like RCLONE_FLAGS, don't export to subshells/child scripts
source /root/bin/envsetup.sh

# Connection monitor: poll the archive endpoint every ~10s. Five
# consecutive misses kill rclone (and this script) so archiveloop can
# reach `connect_usb_drives_to_host` and put the gadget back online
# instead of hanging on a dropped TCP/cloud connection while the user
# drives away. The `--timeout`/`--contimeout` flags below give rclone
# its own internal floor; the monitor is a hard outer bound for cases
# where rclone's retry loop takes too long to surrender.
function connectionmonitor {
  while true
  do
    for _ in {1..5}
    do
      if timeout 6 /root/bin/archive-is-reachable.sh "${ARCHIVE_SERVER:-8.8.8.8}"
      then
        sleep 5
        continue 2
      fi
      sleep 1
    done
    log "connection dead, killing rclone archive"
    killall rclone || true
    sleep 2
    killall -9 rclone || true
    kill -9 "$1" || true
    return
  done
}

connectionmonitor $$ &

# Layer-1 (rclone-level) safety nets. The bash monitor is layer-2.
flags=("-L" "--transfers=1" "--timeout=30s" "--contimeout=10s" "--retries=1")
if [[ -v RCLONE_FLAGS ]]
then
  flags+=("${RCLONE_FLAGS[@]}")
fi

while [ -n "${1+x}" ]
do
  # Low I/O + CPU priority so the archive reads never starve the car's
  # dashcam writes on the same disk (see run/rsync_archive/archive-clips.sh
  # for the full rationale; -c2 -n7 not -c3 so progress is guaranteed).
  ionice -c2 -n7 nice -n19 rclone --config /root/.config/rclone/rclone.conf move "${flags[@]}" --files-from "$2" "$1" "$RCLONE_DRIVE:$RCLONE_PATH" >> "$LOG_FILE" 2>&1
  shift 2
done

# Stop the monitor so it doesn't leak past archive completion.
kill %1 || true
