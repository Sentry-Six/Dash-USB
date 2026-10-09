#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin"

# Exercise the shipped function, redirecting every executable to fixtures.
# Do not source the whole patch script: its other patches target real hardware.
function_source=$(awk '/^apply_bundled_archive_runtime\(\) \{/ { keep=1 } keep { print } keep && /^\}$/ { exit }' "$root/setup/pi/apply-runtime-patches.sh")
[[ -n "$function_source" ]]
function_source=${function_source//\/opt\/dashusb/$scratch/bin}
function_source=${function_source//\/root\/bin\/remountfs_rw/$scratch/remountfs_rw}
eval "$function_source"

export PATCH_TEST_LOG="$scratch/commands"
export PATCH_TEST_HAS_REFRESH=1
export PATCH_TEST_REFRESH_RESULT=0
log() { printf '%s\n' "$*" >> "$scratch/messages"; }
cat > "$scratch/remountfs_rw" <<'SH'
#!/usr/bin/env bash
printf 'remount\n' >> "$PATCH_TEST_LOG"
SH
cat > "$scratch/bin/dashusb-current" <<'SH'
#!/usr/bin/env bash
printf '%s %s\n' "${0##*/}" "$*" >> "$PATCH_TEST_LOG"
case "$1" in
  --help)
    if [[ "$PATCH_TEST_HAS_REFRESH" == 1 ]]; then
      printf 'Commands:\n  refresh-archive-runtime\n'
    else
      printf 'Commands:\n  gadget\n  snapshot\n'
    fi
    ;;
  refresh-archive-runtime) exit "$PATCH_TEST_REFRESH_RESULT" ;;
  *) exit 64 ;;
esac
SH
chmod +x "$scratch/remountfs_rw" "$scratch/bin/dashusb-current"

# An existing updater reaches the newly installed binary's offline refresh.
apply_bundled_archive_runtime
[[ $(cat "$PATCH_TEST_LOG") == $'dashusb-current --help\nremount\ndashusb-current refresh-archive-runtime' ]]
[[ $(cat "$scratch/messages") == 'Installed archive helpers bundled with the running release' ]]

# A downgrade's older binary has no subcommand; do not run unknown commands.
: > "$PATCH_TEST_LOG"
: > "$scratch/messages"
PATCH_TEST_HAS_REFRESH=0
apply_bundled_archive_runtime
[[ $(cat "$PATCH_TEST_LOG") == 'dashusb-current --help' ]]
[[ ! -s "$scratch/messages" ]]

# A refresh failure reaches run_patch's failure accounting, never a success log.
: > "$PATCH_TEST_LOG"
PATCH_TEST_HAS_REFRESH=1
PATCH_TEST_REFRESH_RESULT=27
result=0
apply_bundled_archive_runtime || result=$?
[[ "$result" == 1 ]]
[[ $(cat "$PATCH_TEST_LOG") == $'dashusb-current --help\nremount\ndashusb-current refresh-archive-runtime' ]]
[[ ! -s "$scratch/messages" ]]

# Legacy layouts without dashusb-current use the supported plain binary.
: > "$PATCH_TEST_LOG"
PATCH_TEST_REFRESH_RESULT=0
mv "$scratch/bin/dashusb-current" "$scratch/bin/dashusb"
apply_bundled_archive_runtime
[[ $(cat "$PATCH_TEST_LOG") == $'dashusb --help\nremount\ndashusb refresh-archive-runtime' ]]

echo 'Bundled runtime patch: new and old binaries, failure propagation, and legacy layout passed'
