#!/bin/bash -eu

# Bound quietly dropped SSH links that rsync's socket-idle timeout misses.
MONITOR_MISSES=5
MONITOR_TIMEOUT=6
# Resume partial files after low-priority I/O triggers rsync's timeout.
RSYNC_EXTRA=(--partial)

function connectionmonitor {
  while true
  do
    for (( i = 1; i <= MONITOR_MISSES; i++ ))
    do
      if timeout "$MONITOR_TIMEOUT" /root/bin/archive-is-reachable.sh "$ARCHIVE_SERVER"
      then
        sleep 5
        continue 2
      fi
      sleep 1
    done
    log "connection dead, killing archive-clips"
    # Let rsync delete copied source files before forced termination.
    killall rsync || true
    sleep 2
    killall -9 rsync || true
    kill -9 "$1" || true
    return
  done
}

connectionmonitor $$ &

while [ -n "${1+x}" ]
do
  # Best-effort I/O lets vehicle writes win while guaranteeing archive progress;
  # idle class could starve indefinitely under continuous writes.
  if ! (ionice -c2 -n7 nice -n19 rsync -avhRL --timeout=600 --remove-source-files --no-perms --omit-dir-times \
        ${RSYNC_EXTRA[@]+"${RSYNC_EXTRA[@]}"} \
        --stats --log-file=/tmp/archive-rsync-cmd.log --ignore-missing-args \
        --files-from="$2" "$1" "$RSYNC_USER@$RSYNC_SERVER:$RSYNC_PATH" &> /tmp/rsynclog || [[ "$?" = "24" ]] )
  then
    cat /tmp/archive-rsync-cmd.log /tmp/rsynclog > /tmp/archive-error.log
    kill %1 || true
    exit 1
  fi
  shift 2
done

# Stop the monitor.
kill %1 || true
