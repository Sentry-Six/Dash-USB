#!/bin/bash
# Sourced by archiveloop. Start workers after a late disk mount, and restart
# a failed worker without disrupting its healthy sibling.
camdiskworkers() (
  if [ -w /proc/self/comm ]; then echo -n camdiskworkers > /proc/self/comm; fi
  local snapshot_pid='' space_pid=''
  local delay=${CAM_WORKER_POLL_SECONDS:-10}
  # Invoked by the EXIT/TERM/INT traps below.
  # shellcheck disable=SC2329
  cleanup_workers() {
    trap - EXIT INT TERM
    [ -z "$snapshot_pid" ] || kill "$snapshot_pid" 2>/dev/null || true
    [ -z "$space_pid" ] || kill "$space_pid" 2>/dev/null || true
    [ -z "$snapshot_pid" ] || wait "$snapshot_pid" 2>/dev/null || true
    [ -z "$space_pid" ] || wait "$space_pid" 2>/dev/null || true
  }
  trap cleanup_workers EXIT
  trap 'exit 0' INT TERM
  until has_cam_disk; do sleep "$delay"; done
  snapshotloop & snapshot_pid=$!
  freespacemanager & space_pid=$!
  while true; do
    sleep "$delay"
    if ! kill -0 "$snapshot_pid" 2>/dev/null; then
      wait "$snapshot_pid" 2>/dev/null || true
      log 'Snapshot worker exited unexpectedly; restarting'
      snapshotloop & snapshot_pid=$!
    fi
    if ! kill -0 "$space_pid" 2>/dev/null; then
      wait "$space_pid" 2>/dev/null || true
      log 'Storage cleanup worker exited unexpectedly; restarting'
      freespacemanager & space_pid=$!
    fi
  done
)
