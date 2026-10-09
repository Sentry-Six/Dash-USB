#!/bin/bash
# Raw-HCI ADV_IND fallback for Broadcom controllers that reject or misconfigure
# BlueZ extended advertising.
#
# Apps require the DashUSB-prefixed scan-response name. Never reassert while a
# central connection flag is fresh.
CONNECTING_FLAG="/tmp/ble_connecting"
CONNECTING_FLAG_MAX_AGE=15   # seconds; ignore a stale flag a crashed connect left behind

UUID_LE="9e ca dc 24 0e e5 a9 e0 93 f3 a3 b5 01 00 40 6e"   # 6e400001-b5a3-f393-e0a9-e50e24dcca9e, little-endian
ADV_DATA="15 02 01 06 11 07 ${UUID_LE} 00 00 00 00 00 00 00 00 00 00 00 00 00"

# Scan response: [length][0x09 Complete Local Name][name].
build_scanrsp() {
    local name hex namebytes len
    name=$(timeout 5 btmgmt info 2>/dev/null | sed -n 's/^[[:space:]]*name[[:space:]]*//p' | head -1)
    [ -z "$name" ] && name=$(hostname)
    name=${name:0:29}                                   # scan-rsp budget: 31 - 2 (len+type)
    hex=$(printf '%s' "$name" | od -An -tx1 | tr -d ' \n')
    namebytes=$(( ${#hex} / 2 ))
    len=$(( namebytes + 1 ))                             # +1 for the AD type byte
    printf '%02x 09 %s' "$len" "$(echo "$hex" | sed 's/../& /g')"
}

# Program connectable ADV_IND on all three channels at 100 ms. Advertising type
# byte 0x00 must not become non-connectable ADV_SCAN_IND 0x02.
assert_raw_adv() {
    local scanrsp; scanrsp=$(build_scanrsp)
    hcitool -i hci0 cmd 0x08 0x000a 00 >/dev/null 2>&1 || true
    hcitool -i hci0 cmd 0x08 0x0008 $ADV_DATA >/dev/null 2>&1 || true
    hcitool -i hci0 cmd 0x08 0x0009 $scanrsp 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 >/dev/null 2>&1 || true
    hcitool -i hci0 cmd 0x08 0x0006 a0 00 a0 00 00 00 00 00 00 00 00 00 00 07 00 >/dev/null 2>&1 || true
    # Re-check immediately before enabling to avoid re-arming mid-connect.
    connect_in_flight && return 0
    hcitool -i hci0 cmd 0x08 0x000a 01 >/dev/null 2>&1 || true
}

# True while a central connect is in flight (flag fresh within max-age).
connect_in_flight() {
    local mtime now age
    mtime=$(stat -c %Y "$CONNECTING_FLAG" 2>/dev/null) || return 1
    now=$(date +%s)
    age=$(( now - mtime ))
    [ "$age" -ge 0 ] && [ "$age" -lt "$CONNECTING_FLAG_MAX_AGE" ]
}

# Sourcing exposes helpers without starting the service loop.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    return 0 2>/dev/null || exit 0
fi

for i in $(seq 1 30); do busctl status org.bluez >/dev/null 2>&1 && break; sleep 1; done
sleep 3   # let bluetoothd's (failing) ext-adv RegisterAdvertisement settle first
timeout 5 btmgmt le on >/dev/null 2>&1 || true
timeout 5 btmgmt connectable on >/dev/null 2>&1 || true
while true; do
    # Re-assert only while IDLE and no central connect is in flight.
    if ! connect_in_flight && ! timeout 3 btmgmt con 2>/dev/null | grep -q 'type LE'; then
        assert_raw_adv
    fi
    sleep 5
done
