#!/bin/bash
# `connect_usb_drives_to_host` re-enumerates the gadget. These tests require
# `ensure_usb_drives_connected` to call it only while the gadget is down.
#
# Usage: bash test/archiveloop-gadget-cycle.test.sh [path/to/archiveloop]
set -euo pipefail

script=${1:-run/archiveloop}

# Exercise function bodies extracted from the shipped script.
eval "$(awk '
  /^function ensure_usb_drives_connected / {keep=1}
  keep {print}
  /^function trim_free_space/ {exit}
' "$script" | sed '$d')"

if ! declare -F ensure_usb_drives_connected >/dev/null \
  || ! declare -F ensure_usb_drives_connected_locked >/dev/null; then
  echo "FAIL: could not extract ensure_usb_drives_connected* from $script" >&2
  exit 1
fi

connect_calls=0
usb_active=true
logs=""

log() { logs+="$*"$'\n'; }
usb_gadget_is_active() { [ "$usb_active" = true ]; }
connect_usb_drives_to_host_locked() {
  connect_calls=$((connect_calls + 1))
  usb_active=true
}
# Lock ordering is outside these connection-state tests.
with_gadget_lock() { "$@"; }

failures=0
check() {
  local label=$1 want=$2 got=$3
  if [ "$want" = "$got" ]; then
    echo "ok   - $label"
  else
    echo "FAIL - $label (want $want, got $got)" >&2
    failures=$((failures + 1))
  fi
}

reset() {
  connect_calls=0
  logs=""
}

# An already-presented drive must not be cycled.
reset
usb_active=true
ensure_usb_drives_connected
check "active gadget is left alone" 0 "$connect_calls"
case "$logs" in
  *"skipping redundant reconnect"*) echo "ok   - logs the skip" ;;
  *) echo "FAIL - expected a skip log, got: $logs" >&2; failures=$((failures + 1)) ;;
esac

# A gadget that is genuinely down must still be recovered.
reset
usb_active=false
ensure_usb_drives_connected
check "inactive gadget is reconnected" 1 "$connect_calls"
check "gadget ends up active" true "$usb_active"

# Multiple calls in one cycle must produce at most one connection.
reset
usb_active=false
ensure_usb_drives_connected   # post-archive (run_archive_cycle)
ensure_usb_drives_connected   # outer loop
check "double call in one cycle connects once" 1 "$connect_calls"

# A zero-clip cycle never disconnects and therefore must not reconnect.
reset
usb_active=true
ensure_usb_drives_connected
ensure_usb_drives_connected
check "zero-clip cycle never re-enumerates" 0 "$connect_calls"

if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all gadget-cycle tests passed"
