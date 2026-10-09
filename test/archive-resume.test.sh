#!/usr/bin/env bash
# Execute the shipped helpers and recording pipeline against temporary GM
# recording links. Only mounts, transfers and notification delivery are stubbed.
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tmp" "$work/mutable" "$work/bin"
extract() {
  awk -v name="$1" '$0 ~ "^function " name "([ (]|$)" {keep=1} keep {print} keep && /^}/ {exit}' run/archiveloop
}
rewrite_fixture_paths() {
  # Replace each original prefix once. On Linux the fixture itself is under
  # /tmp, so sequential substitutions would rewrite an already-inserted path.
  python3 -c 'import re, sys
root = sys.argv[1]
paths = {"/root/bin/": root + "/bin/", "/mutable/": root + "/mutable/", "/tmp/": root + "/tmp/"}
sys.stdout.write(re.sub(r"/root/bin/|/mutable/|/tmp/", lambda match: paths[match.group()], sys.stdin.read()))' "$work"
}
for name in archive_start_summary archive_finish_summary archive_resume_context write_archive_checkpoint finish_archive_checkpoint archive_progress_monitor archive_recordings sortfile intersect prunefile filterfile convert_seconds_to_nice_time cancel_archive_cleanup; do
  eval "$(extract "$name" | rewrite_fixture_paths)"
done
checkpoint="$work/mutable/archive_in_progress.json"
printf '%s\n' 'Continuous/a.mp4' 'Continuous/b.mp4' 'Continuous/c.mp4' > "$work/full"
printf '%s\n' 'Continuous/c.mp4' 'Continuous/a.mp4' 'Continuous/b.mp4' > "$work/reordered"
printf '%s\n' 'Continuous/a.mp4' 'Continuous/b.mp4' 'Continuous/new.mp4' > "$work/unrelated"
printf '%s\n' 'Continuous/b.mp4' 'Continuous/c.mp4' > "$work/remaining"
printf '%s\n' 'Continuous/c.mp4' > "$work/remaining-again"
read -r fingerprint completed offset total < <(archive_resume_context "$work/full" 3 "$checkpoint")
[[ "$completed $offset $total" == '0 0 3' ]]
write_archive_checkpoint '{"phase":"archiving","current":1,"total":3}' "$fingerprint" 0 3 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/reordered" 3 "$checkpoint")
[[ "$key $completed $offset $total" == "$fingerprint 1 0 3" ]]
[[ "$(archive_start_summary 3 "$completed" "$total")" == 'Resuming archive: 1 of 3 recordings archived.' ]]
read -r key completed offset total < <(archive_resume_context "$work/unrelated" 3 "$checkpoint")
[[ "$completed $offset $total" == '0 0 3' ]]

# A handled disconnect already committed completed paths to the dedup ledger.
finish_archive_checkpoint false false "$work/remaining" 2 1 3 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining" 2 "$checkpoint")
[[ "$completed $offset $total" == '1 1 3' ]]
# An abrupt reboot adds verified in-flight progress without double-subtracting it.
write_archive_checkpoint '{"current":1,"total":2}' "$key" "$offset" "$total" "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining" 2 "$checkpoint")
[[ "$completed $offset $total" == '2 1 3' ]]
finish_archive_checkpoint false false "$work/remaining-again" 1 2 3 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining-again" 1 "$checkpoint")
[[ "$completed $offset $total" == '2 2 3' ]]

for invalid in '{"current":1,"total":3}' 'not JSON' 'null' '[]' '{"candidate_hash":false}' ; do
  printf '%s' "$invalid" > "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 3 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 3' ]]
done
for current in -1 4 null true '1.5' '"1"'; do
  write_archive_checkpoint "{\"current\":$current,\"total\":3}" "$fingerprint" 0 3 "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 3 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 3' ]]
done
for counts in '1 3' '-1 2' '1.5 4.5' '9223372036854775808 9223372036854775811'; do
  read -r previous original <<< "$counts"
  write_archive_checkpoint '{"current":1,"total":3}' "$fingerprint" "$previous" "$original" "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 3 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 3' ]]
done
for outcome in 'true false 2' 'false true 2' 'false false 0'; do
  touch "$checkpoint" "$checkpoint.tmp"
  read -r success cancelled remaining <<< "$outcome"
  finish_archive_checkpoint "$success" "$cancelled" "$work/remaining" "$remaining" 1 3 "$checkpoint"
  [[ ! -e "$checkpoint" && ! -e "$checkpoint.tmp" ]]
done
# A remainder count cannot claim a different list; discard old evidence.
touch "$checkpoint"
if finish_archive_checkpoint false false "$work/remaining" 3 0 3 "$checkpoint" 2>/dev/null; then
  echo 'accepted a mismatched remainder count' >&2; exit 1
fi
[[ ! -e "$checkpoint" ]]
printf '%s\n' "Continuous/\$(touch $work/injected).mp4" > "$work/literal"
archive_resume_context "$work/literal" 1 "$checkpoint" >/dev/null
[[ ! -e "$work/injected" ]]

# Monitor writes preserve the verified prior offset while measuring only this
# attempt's actual disappearing overlay links.
mkdir "$work/overlay"
ln -s "$work/source" "$work/overlay/b.mp4"
ln -s "$work/source" "$work/overlay/c.mp4"
printf '%s\n' b.mp4 c.mp4 > "$work/monitor-list"
read -r monitor_hash _ < <(archive_resume_context "$work/monitor-list" 2 /dev/null)
(
  step=0
  sleep() {
    step=$((step + 1))
    [[ "$step" == 1 ]] || exit 0
    rm "$work/overlay/b.mp4"
  }
  write_archive_status() { printf '%s\n' "$1" > "$work/status"; }
  archive_progress_monitor 2 "$work/monitor-list" "$work/overlay" "$monitor_hash" 1 3
)
read -r key completed offset total < <(archive_resume_context "$work/monitor-list" 2 "$checkpoint")
[[ "$completed $offset $total" == '2 1 3' ]]
rm "$checkpoint"

# Real archive_recordings integration: failures persist a shrinking candidate
# set, successive reconnects carry the original total, success/cancel clear it.
export RECORDINGS_TREE="$work/mutable/Recordings"
export LOG_FILE="$work/log"
export NOTIFICATIONS="$work/notifications"
export NOTIFICATION_TITLE=DashUSB
export CLIP_MIN_BYTES=0
export CAM_CLEANUP_DEFERRED="$work/mutable/cleanup-deferred"
mkdir -p "$RECORDINGS_TREE/Continuous/day" "$work/originals"
for name in a b c d e; do
  printf 'immutable recording %s\n' "$name" > "$work/originals/$name.mp4"
  ln -s "$work/originals/$name.mp4" "$RECORDINGS_TREE/Continuous/day/$name.mp4"
done
cat > "$work/bin/send-push-message" <<'SCRIPT'
#!/bin/bash
printf '%s|%s|%s\n' "$3" "$4" "$5" >> "$NOTIFICATIONS"
SCRIPT
for command in send-live-activity disconnect-archive.sh; do
  printf '#!/bin/bash\nexit 0\n' > "$work/bin/$command"
done
chmod +x "$work/bin/"*
log() { printf '%s\n' "$*" >> "$LOG_FILE"; }
mount() { local target="${!#}"; cp -a "$RECORDINGS_TREE/." "$target/"; }
umount() { :; }
# macOS find lacks -fprintf; preserve exactly the production query's behavior
# for fixture links. Linux runs the real GNU command unchanged.
if ! find --version >/dev/null 2>&1; then
  find() {
    [[ "$#" == 8 && "$1" == . && "$2" == -path && "$3" == './Continuous/*' && "$4" == -type && "$5" == l && "$6" == -fprintf && "$8" == '%P\n' ]] || return 99
    python3 - "$7" <<'PYFIND'
import os, sys
from pathlib import Path
with Path(sys.argv[1]).open("w") as output:
    for folder, _, names in os.walk("Continuous"):
        for name in names:
            path = Path(folder) / name
            if path.is_symlink():
                output.write(str(path) + "\n")
PYFIND
  }
fi
if ! xargs --version >/dev/null 2>&1; then
  xargs() {
    [[ "$#" == 7 && "$1" == -a && "$3" == -d && "$4" == '\n' && "$5" == stat && "$6" == -L && "$7" == '--format=%s %n' ]] || return 99
    local line
    while IFS= read -r line; do
      command stat -L --format='%s %n' "$line"
    done < "$2"
  }
fi
archive_is_reachable() { return 0; }
travel_mode_active() { return 0; } # Keep cleanup deferred without real car mounts.
soc_temperature() { :; }
write_archive_status() { printf '%s\n' "$1" > "$work/status"; }
archive_cancel_requested() { [[ -e "$work/cancelled" ]]; }
archive_run_command() {
  local overlay=$2 list=$3 line count=0
  while IFS= read -r line; do
    [[ "$MODE" == success || "$count" -lt "$MOVE_COUNT" ]] || break
    rm "$overlay/$line"
    count=$((count + 1))
  done < "$list"
  if [[ "$MODE" == cancel ]]; then touch "$work/cancelled"; return 125; fi
  [[ "$MODE" == success ]]
}
clear_archive_status() { :; }
ensure_usb_drives_connected() { :; }

MODE=failure MOVE_COUNT=2
rc=0
archive_recordings || rc=$?
[[ "$rc" == 1 ]]
read -r key completed offset total < <(archive_resume_context "$work/tmp/recording_files_remaining" 3 "$checkpoint")
[[ "$completed $offset $total" == '2 2 5' ]]
grep -Fxq 'start|archive_start|Archiving 5 recordings.' "$NOTIFICATIONS"
grep -Fxq 'error|archive_error|Archive interrupted: 2 of 5 recordings archived; 3 remaining.' "$NOTIFICATIONS"
MODE=failure MOVE_COUNT=1
rc=0
archive_recordings || rc=$?
[[ "$rc" == 1 ]]
read -r key completed offset total < <(archive_resume_context "$work/tmp/recording_files_remaining" 2 "$checkpoint")
[[ "$completed $offset $total" == '3 3 5' ]]
grep -Fxq 'start|archive_start|Resuming archive: 2 of 5 recordings archived.' "$NOTIFICATIONS"
MODE=success
archive_recordings
[[ ! -e "$checkpoint" ]]
grep -Fxq 'start|archive_start|Resuming archive: 3 of 5 recordings archived.' "$NOTIFICATIONS"
grep -Fxq 'finish|archive_complete|5 recordings archived.' "$NOTIFICATIONS"
# Empty candidates clear even a legacy checkpoint; no false resume is sent.
before=$(wc -l < "$NOTIFICATIONS")
printf '{"current":99,"total":100}' > "$checkpoint"
touch "$checkpoint.tmp"
archive_recordings
[[ ! -e "$checkpoint" && ! -e "$checkpoint.tmp" ]]
[[ "$(wc -l < "$NOTIFICATIONS")" == "$before" ]]

for name in f g; do
  printf 'immutable recording %s\n' "$name" > "$work/originals/$name.mp4"
  ln -s "$work/originals/$name.mp4" "$RECORDINGS_TREE/Continuous/day/$name.mp4"
done
MODE=cancel MOVE_COUNT=1
rc=0
archive_recordings || rc=$?
[[ "$rc" == 125 && ! -e "$checkpoint" ]]
grep -Fxq 'finish|archive_cancelled|Archive stopped. Remaining footage kept.' "$NOTIFICATIONS"
# Teardown cancellation also clears a checkpoint written earlier in the cycle.
touch "$checkpoint" "$checkpoint.tmp"
cancel_archive_cleanup
[[ ! -e "$checkpoint" && ! -e "$checkpoint.tmp" && -f "$CAM_CLEANUP_DEFERRED" ]]
for name in a b c d e f g; do
  [[ "$(cat "$work/originals/$name.mp4")" == "immutable recording $name" ]]
  [[ -L "$RECORDINGS_TREE/Continuous/day/$name.mp4" ]]
done
echo 'archive resume: verified/reordered candidates, malformed/stale data, monitor offset, reconnects, success and cancellation passed'
