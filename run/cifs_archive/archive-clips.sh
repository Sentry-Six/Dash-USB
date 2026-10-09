#!/bin/bash -eu

source /root/bin/mounted-archive-monitor.sh

rm -f /tmp/archive-rsync-cmd.log /tmp/archive-error.log

connectionmonitor $$ &

# Isolate temporary files so interrupted transfers can be cleaned.
rsynctmp=".dashusbtmp"
rm -rf "$ARCHIVE_MOUNT/${rsynctmp:?}" || true
mkdir -p "$ARCHIVE_MOUNT/$rsynctmp"


while [ -n "${1+x}" ]
do
  # Best-effort I/O preserves vehicle writes without starving the archive.
  if ! (ionice -c2 -n7 nice -n19 rsync -avhRL --remove-source-files --temp-dir="$rsynctmp" --no-perms --omit-dir-times --stats \
        --log-file=/tmp/archive-rsync-cmd.log --log-file-format='%i %b %n%L' --ignore-missing-args \
        --files-from="$2" "$1/" "$ARCHIVE_MOUNT" &> /tmp/rsynclog || [[ "$?" = "24" ]] )
  then
    cat /tmp/archive-rsync-cmd.log /tmp/rsynclog > /tmp/archive-error.log
    exit 1
  fi

  shift 2
done

rm -rf "$ARCHIVE_MOUNT/${rsynctmp:?}" || true

kill %1 || true
