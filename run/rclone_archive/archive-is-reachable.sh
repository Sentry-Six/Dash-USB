#!/bin/bash -eu

# S3 endpoints behind a proxy serve HTTPS but drop ICMP, so ping alone reports
# a working bucket dead. archive-clips.sh also runs this mid-transfer and
# SIGTERMs rclone after five failures, so the total budget MUST stay under its
# 6s wrapper: a filtered port burns its whole timeout.
#
# Never interpolate the target into shell code. It is user config, and this
# runs as root.

TARGET="$1"

# Prefer the remote's own endpoint: it is what rclone connects to, port
# included, so it cannot disagree with ARCHIVE_SERVER. Remotes without one
# (Drive, Dropbox, AWS S3) fall back to the passed-in target.
RCLONE_CONF="${RCLONE_CONFIG:-/root/.config/rclone/rclone.conf}"
if [ -n "${RCLONE_DRIVE:-}" ] && [ -r "$RCLONE_CONF" ]; then
    endpoint=$(awk -v d="$RCLONE_DRIVE" '
        /^[[:space:]]*\[/ { s = ($0 ~ "^[[:space:]]*\\["d"\\][[:space:]]*$") }
        s && /^[[:space:]]*endpoint[[:space:]]*=/ {
            sub(/^[^=]*=[[:space:]]*/, ""); gsub(/[[:space:]]+$/, ""); print; exit
        }' "$RCLONE_CONF" 2>/dev/null) || endpoint=""
    [ -n "$endpoint" ] && TARGET="$endpoint"
fi

# Split scheme, host, and optional port. Handles [::1]:443 and bare IPv6.
REST="$TARGET"
SCHEME=""
case "$REST" in
    *://*) SCHEME="${REST%%://*}"; REST="${REST#*://}" ;;
esac
REST="${REST%%/*}"

case "$REST" in
    \[*\]*)                       # bracketed IPv6, optional :port
        HOST="${REST%%\]*}"; HOST="${HOST#\[}"
        PORT="${REST##*\]}"; PORT="${PORT#:}" ;;
    *:*:*)  HOST="$REST"; PORT="" ;;   # bare IPv6, no port possible
    *:*)    HOST="${REST%%:*}"; PORT="${REST##*:}" ;;
    *)      HOST="$REST"; PORT="" ;;
esac
[ -n "$HOST" ] || exit 1

is_port () { case "$1" in ''|*[!0-9]*) return 1 ;; esac; [ "$1" -ge 1 ] && [ "$1" -le 65535 ]; }

# An explicit port is probed exclusively. Falling back to 443/80 could call
# the endpoint healthy because an unrelated service answered.
if is_port "${PORT:-}"; then
    PORTS="$PORT"
else
    case "$SCHEME" in
        https) PORTS="443" ;;
        http)  PORTS="80" ;;
        *)     PORTS="443 80" ;;
    esac
fi

PING_TIMEOUT="${ARCHIVE_PING_TIMEOUT:-1}"
is_port "$PING_TIMEOUT" || PING_TIMEOUT=1
TCP_TIMEOUT="${ARCHIVE_TCP_TIMEOUT:-2}"
is_port "$TCP_TIMEOUT" || TCP_TIMEOUT=2

if ping -q -w "$PING_TIMEOUT" -c 1 -- "$HOST" > /dev/null 2>&1; then
    exit 0
fi

# Setup installs netcat-openbsd, so nc is the normal path. /dev/tcp runs only
# when nc is absent, and only for a strict charset: it interpolates.
tcp_probe () {
    local host="$1" port="$2"
    if command -v nc > /dev/null 2>&1; then
        nc -z -w "$TCP_TIMEOUT" -- "$host" "$port" > /dev/null 2>&1
        return
    fi
    case "$host" in
        *[!A-Za-z0-9.:-]*) return 1 ;;
    esac
    timeout "$TCP_TIMEOUT" bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null
}

for port in $PORTS; do
    is_port "$port" || continue
    if tcp_probe "$HOST" "$port"; then
        exit 0
    fi
done

exit 1
