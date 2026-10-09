#!/usr/bin/env bash
# Exercise the installed monitor with deterministic time/probes and actual
# rsync log records. Signals are captured: no real process is terminated.
set -euo pipefail
cd "$(dirname "$0")/.."
source run/mounted-archive-monitor.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

run_scenario() (
  scenario=$1
  scenario_dir="$work/$scenario"
  mkdir "$scenario_dir"
  MONITOR_RSYNC_LOG="$scenario_dir/rsync.log"
  : > "$MONITOR_RSYNC_LOG"
  : > "$scenario_dir/signals"
  : > "$scenario_dir/messages"
  export ARCHIVE_SERVER=nas.invalid
  export TRAVEL_MODE_ACTIVE=0
  unset ARCHIVE_STALL_GRACE_SECONDS
  [ "$scenario" != travel_stall ] || export TRAVEL_MODE_ACTIVE=1
  clock=100

  date() {
    [ "$*" = '+%s' ] || exit 90
    printf '%s\n' "$clock"
  }
  timeout() {
    [ "$*" = '6 /root/bin/archive-is-reachable.sh nas.invalid' ] || exit 91
    case "$scenario" in
      transient_progress)
        # Ninety seconds of failed SYN probes would kill the old watchdog.
        # Actual completed transfers keep this established session alive.
        if [ $((clock % 10)) = 0 ] && [ "$clock" -lt 190 ]; then
          printf '2026-10-09 12:00:00 >f+++++++++ 1024 Continuous/clip-%s.mp4\n' "$clock" >> "$MONITOR_RSYNC_LOG"
        fi
        [ "$clock" -ge 190 ] && return 0
        ;;
      continuous_failure)
        # Cached completions must not defeat the 300-second departure cap.
        printf '2026-10-09 12:00:00 >f+++++++++ 1024 Continuous/clip-%s.mp4\n' "$clock" >> "$MONITOR_RSYNC_LOG"
        ;;
      genuine_stall|travel_stall)
        # Queued filenames, directory records and vanished-file warnings
        # are not completed transfers and must not postpone a real stall.
        printf 'Continuous/queued-%s.mp4\n2026-10-09 12:00:00 cd+++++++++ 0 Continuous/\nfile vanished: missing.mp4\n' "$clock" >> "$MONITOR_RSYNC_LOG"
        ;;
      *) exit 92 ;;
    esac
    return 1
  }
  sleep() {
    case "$1" in 2|5) ;; *) exit 93 ;; esac
    clock=$((clock + $1))
    # Stop the deliberately healthy infinite monitor after three minutes.
    if [ "$scenario" = transient_progress ] && [ "$clock" -ge 280 ]; then exit 42; fi
    [ "$clock" -lt 500 ] || exit 94
  }
  log() { printf '%s\n' "$*" >> "$scenario_dir/messages"; }
  pkill() {
    # Both TERM and KILL must target only this archive's exact log marker.
    case "$*" in
      '-f rsync .*--log-file=/tmp/archive-rsync-cmd\.log'|'-9 -f rsync .*--log-file=/tmp/archive-rsync-cmd\.log') ;;
      *) echo 'unscoped process termination' >&2; exit 95 ;;
    esac
    printf '%s pkill %s\n' "$clock" "$*" >> "$scenario_dir/signals"
  }
  kill() {
    [ "$*" = '-9 424242' ] || exit 96
    printf '%s kill %s\n' "$clock" "$*" >> "$scenario_dir/signals"
  }
  killall() { echo 'monitor attempted killall' >&2; exit 97; }
  connectionmonitor 424242
)

rc=0
run_scenario transient_progress || rc=$?
[ "$rc" = 42 ] || { echo "healthy monitor stopped unexpectedly: $rc" >&2; exit 1; }
[ ! -s "$work/transient_progress/signals" ] || { echo 'progressing/recovered session was killed' >&2; exit 1; }
[ ! -s "$work/transient_progress/messages" ]

assert_termination() {
  local scenario=$1 elapsed=$2 stalled=$3
  run_scenario "$scenario"
  local signals="$work/$scenario/signals"
  [ "$(wc -l < "$signals" | tr -d ' ')" = 3 ]
  # SIGTERM gets a two-second grace before scoped SIGKILL and parent exit.
  [ "$(sed -n '1p' "$signals")" = "$((100 + elapsed)) pkill -f rsync .*--log-file=/tmp/archive-rsync-cmd\.log" ]
  [ "$(sed -n '2p' "$signals")" = "$((102 + elapsed)) pkill -9 -f rsync .*--log-file=/tmp/archive-rsync-cmd\.log" ]
  [ "$(sed -n '3p' "$signals")" = "$((102 + elapsed)) kill -9 424242" ]
  grep -Fq "probes failing ${elapsed}s, no completed file for ${stalled}s" "$work/$scenario/messages"
}
assert_termination genuine_stall 60 60
assert_termination travel_stall 180 180
assert_termination continuous_failure 300 0

# %b makes rsync write a completion record at transfer END; default log
# formatting would mistake queued files for progress. Check both consumers.
for backend in cifs nfs; do
  grep -Fq 'source /root/bin/mounted-archive-monitor.sh' "run/${backend}_archive/archive-clips.sh"
  grep -Fq -- "--log-file-format='%i %b %n%L'" "run/${backend}_archive/archive-clips.sh"
done

echo 'mounted archive monitor: progressing link, real stall, Travel Mode grace, hard cap and scoped termination passed'
