#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
source run/cam-disk-supervisor.sh
scratch=$(mktemp -d)
supervisor=''
cleanup() {
  [ -z "$supervisor" ] || kill "$supervisor" 2>/dev/null || true
  [ -z "$supervisor" ] || wait "$supervisor" 2>/dev/null || true
  rm -rf "$scratch"
}
trap cleanup EXIT
log() { echo "$*" >> "$scratch/log"; }
has_cam_disk() { [ -f "$scratch/disk-ready" ]; }
snapshotloop() {
  echo snapshot >> "$scratch/snapshots"
  while true; do sleep 0.05; done
}
freespacemanager() {
  echo space >> "$scratch/space"
  if [ "$(wc -l < "$scratch/space")" -eq 1 ]; then exit 7; fi
  while true; do sleep 0.05; done
}
CAM_WORKER_POLL_SECONDS=0.05 camdiskworkers & supervisor=$!
sleep 0.15
[ ! -e "$scratch/snapshots" ] || { echo 'workers started before disk was mounted'; exit 1; }
touch "$scratch/disk-ready"
for _ in {1..100}; do
  if [ -e "$scratch/space" ] && [ "$(wc -l < "$scratch/space")" -ge 2 ]; then break; fi
  sleep 0.05
done
[ "$(wc -l < "$scratch/snapshots")" -eq 1 ] || { echo 'healthy snapshot worker was needlessly restarted'; exit 1; }
[ "$(wc -l < "$scratch/space")" -ge 2 ] || { echo 'failed cleanup worker never restarted'; exit 1; }
kill -0 "$supervisor"
case "$(cat "$scratch/log")" in *'Storage cleanup worker exited unexpectedly; restarting'*) ;; *) exit 1 ;; esac
echo 'cam-disk supervisor: late mount and failed-worker recovery passed'
