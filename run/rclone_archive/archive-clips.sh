#!/bin/bash -eu

# Reload arrays such as RCLONE_FLAGS, which cannot be exported.
source /root/bin/envsetup.sh

# Bound dropped cloud connections beyond rclone's internal retry timeouts.
function connectionmonitor {
  while true
  do
    for _ in {1..5}
    do
      # Inherit ARCHIVE_PING_TIMEOUT so a slow cellular link (Travel Mode)
      # is not misread as dead and used to kill an in-flight transfer.
      if ARCHIVE_PING_TIMEOUT="${ARCHIVE_PING_TIMEOUT:-1}" \
         timeout 6 /root/bin/archive-is-reachable.sh "${ARCHIVE_SERVER:-8.8.8.8}"
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
  # Best-effort I/O preserves vehicle writes without starving the archive.
  ionice -c2 -n7 nice -n19 rclone --config /root/.config/rclone/rclone.conf move "${flags[@]}" --files-from "$2" "$1" "$RCLONE_DRIVE:$RCLONE_PATH" >> "$LOG_FILE" 2>&1
  shift 2
done

# Stop the monitor so it doesn't leak past archive completion.
kill %1 || true
