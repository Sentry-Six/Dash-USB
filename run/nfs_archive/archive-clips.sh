#!/bin/bash -eu

function connectionmonitor {
  while true
  do
    for _ in {1..5}
    do
      if timeout 6 /root/bin/archive-is-reachable.sh "$ARCHIVE_SERVER"
      then
        sleep 5
        continue 2
      fi
      sleep 1
    done
    log "connection dead, killing archive-clips"
    killall rsync || true
    sleep 2
    killall -9 rsync || true
    kill -9 "$1" || true
    return
  done
}

connectionmonitor $$ &

rsynctmp=".dashusbtmp"
rm -rf "$ARCHIVE_MOUNT/${rsynctmp:?}" || true
mkdir -p "$ARCHIVE_MOUNT/$rsynctmp"

rm -f /tmp/archive-rsync-cmd.log /tmp/archive-error.log

while [ -n "${1+x}" ]
do
  # Using --no-o --no-g to prevent permission errors on NFS root squashed shares
  # Low I/O + CPU priority so the archive reads never starve the car's
  # dashcam writes on the same disk (see run/rsync_archive/archive-clips.sh
  # for the full rationale; -c2 -n7 not -c3 so progress is guaranteed).
  if ! (ionice -c2 -n7 nice -n19 rsync -avhRL --no-o --no-g --remove-source-files --temp-dir="$rsynctmp" --no-perms --omit-dir-times --stats \
        --log-file=/tmp/archive-rsync-cmd.log --ignore-missing-args \
        --files-from="$2" "$1/" "$ARCHIVE_MOUNT" &> /tmp/rsynclog || [[ "$?" = "24" ]] )
  then
    cat /tmp/archive-rsync-cmd.log /tmp/rsynclog > /tmp/archive-error.log
    exit 1
  fi
  shift 2
done

rm -rf "$ARCHIVE_MOUNT/${rsynctmp:?}" || true
kill %1 || true
