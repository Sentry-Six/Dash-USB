#!/bin/bash
#
# Make Avahi advertise <hostname>.local over IPv4 only. Device IPv6 is
# untouched; this changes only what mDNS hands out.
#
# Windows and Chrome prefer the AAAA answer for .local. The board's global
# SLAAC address rotates, so that answer goes stale and page loads stall until
# IPv4 fallback. Chrome also treats a global IPv6 address as public address
# space, so the plain-http Web UI reached through it hits Private Network
# Access blocks reported as "blocked by CORS policy ... more-restricted
# address space".
#
# Edits are section-aware (a use-ipv6 line under [publish] stops avahi from
# starting), dedupe repeated assignments, and append a missing section. Prints
# "changed" or "unchanged" so the caller knows whether to restart avahi; this
# script never restarts it. Exits nonzero if the conf is missing or an edit
# fails, which callers treat as a warning rather than fatal.

set -eu

# Overridable for tests only; on-device callers use the default.
CONF="${AVAHI_CONF:-/etc/avahi/avahi-daemon.conf}"

if [ ! -f "$CONF" ]; then
  echo "avahi configuration not found: $CONF" >&2
  exit 1
fi

CHANGED=0

# set_key <section> <key> <value>: rewrites $CONF in place, atomically.
# Sets CHANGED=1 when the file was modified. Returns nonzero only on a real
# failure, never for "already correct".
set_key() {
  local section="$1" key="$2" value="$3" tmp
  tmp=$(mktemp "$CONF.dashusb-tmp.XXXXXX") || return 1
  # Carry the conf's owner and mode onto the temp file before filling it, so
  # the rename doesn't change the file's metadata.
  cp -p -- "$CONF" "$tmp" || { rm -f -- "$tmp"; return 1; }
  if ! awk -v section="$section" -v key="$key" -v value="$value" '
    BEGIN { insec = 0; done = 0; foundsec = 0 }
    /^[ \t]*\[/ {
      # Leaving the target section without having placed the key: insert it.
      if (insec && !done) { print key "=" value; done = 1 }
      line = $0
      gsub(/^[ \t]+|[ \t]+$/, "", line)
      insec = (line == "[" section "]")
      if (insec) foundsec = 1
      print
      next
    }
    insec {
      line = $0
      sub(/^[ \t]*[#;]?[ \t]*/, "", line)
      if (line ~ ("^" key "[ \t]*=")) {
        # First active/commented assignment becomes the desired line;
        # any repeats in the same section are dropped.
        if (!done) { print key "=" value; done = 1 }
        next
      }
      print
      next
    }
    { print }
    END {
      if (!done) {
        if (!foundsec) print "[" section "]"
        print key "=" value
      }
    }
  ' "$CONF" > "$tmp"; then
    rm -f -- "$tmp"
    return 1
  fi
  if cmp -s -- "$tmp" "$CONF"; then
    rm -f -- "$tmp"
    return 0
  fi
  # Keep one pristine copy from before our first edit ever.
  [ -f "$CONF.dashusb-prev" ] || cp -p -- "$CONF" "$CONF.dashusb-prev" \
    || { rm -f -- "$tmp"; return 1; }
  # The rename is atomic: a crash mid-edit leaves the previous conf intact.
  mv -f -- "$tmp" "$CONF" || { rm -f -- "$tmp"; return 1; }
  CHANGED=1
}

set_key server use-ipv6 no
set_key publish publish-aaaa-on-ipv4 no

if [ "$CHANGED" = 1 ]; then
  echo "changed"
else
  echo "unchanged"
fi
