#!/bin/bash -eu

function connectionmonitor {
  while true
  do
    for _ in {1..5}
    do
      if timeout 6 /root/bin/archive-is-reachable.sh "$ARCHIVE_SERVER"
      then
        # sleep and then continue outer loop
        sleep 5
        continue 2
      fi
      sleep 1
    done
    log "connection dead, killing archive-clips"
    # Since there can be a substantial delay before rsync deletes archived
    # source files, give it an opportunity to delete them before killing it
    # hard.
    killall rsync || true
    sleep 2
    killall -9 rsync || true
    kill -9 "$1" || true
    return
  done
}

connectionmonitor $$ &

# rsync's temp files may be left behind if the connection is lost,
# but rsync doesn't clean these up on subsequent runs. Putting
# them in a temp dir allows them to be easily cleaned up.
rsynctmp=".dashusbtmp"
rm -rf "$ARCHIVE_MOUNT/${rsynctmp:?}" || true
mkdir -p "$ARCHIVE_MOUNT/$rsynctmp"

rm -f /tmp/archive-rsync-cmd.log /tmp/archive-error.log

while [ -n "${1+x}" ]
do
  # Low I/O + CPU priority so the archive reads never starve the car's
  # dashcam writes on the same disk (see run/rsync_archive/archive-clips.sh
  # for the full rationale; -c2 -n7 not -c3 so progress is guaranteed).
  if ! (ionice -c2 -n7 nice -n19 rsync -avhRL --remove-source-files --temp-dir="$rsynctmp" --no-perms --omit-dir-times --stats \
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
