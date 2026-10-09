#!/bin/bash
# Sourced by archiveloop; uses its log function.

# Six transport messages in ten seconds identify a burst, not a slow transfer.
function wifi_watchdog_init {
  WIFI_TRANSPORT_TIMES='' WIFI_SUPPLICANT_TIMES=''
  WIFI_LAST_FAULT=-1 WIFI_STORM_LATCHED=false WIFI_LAST_ATTEMPT=-600
  WIFI_WATCHDOG_REASON=
}

function wifi_watchdog_observe {
  local line=$1 now=$2 kind='' timestamps='' recent='' sample count=0 threshold
  case "$line" in
    *brcmfmac*CMD53*failed*|*brcmfmac*brcmf_sdio_txfail*|*brcmfmac*RXHEADER\ FAILED*|*brcmfmac*max\ tx\ seq\ number\ error*) kind=transport ;;
    *failed\ to\ enable\ fw\ supplicant*) kind=supplicant ;;
    *) return 1 ;;
  esac
  if [ "$WIFI_LAST_FAULT" -lt 0 ] || [ $((now - WIFI_LAST_FAULT)) -ge 60 ]; then
    WIFI_STORM_LATCHED=false
    WIFI_TRANSPORT_TIMES='' WIFI_SUPPLICANT_TIMES=''
  fi
  WIFI_LAST_FAULT=$now
  [ "$WIFI_STORM_LATCHED" = false ] || return 1
  if [ "$kind" = transport ]; then
    timestamps=$WIFI_TRANSPORT_TIMES threshold=6
  else
    timestamps=$WIFI_SUPPLICANT_TIMES threshold=2
  fi
  for sample in $timestamps; do
    if [ $((now - sample)) -le 10 ]; then
      recent="$recent $sample"
      count=$((count + 1))
    fi
  done
  recent="$recent $now"
  count=$((count + 1))
  if [ "$kind" = transport ]; then WIFI_TRANSPORT_TIMES=$recent; else WIFI_SUPPLICANT_TIMES=$recent; fi
  [ "$count" -ge "$threshold" ] || return 1
  WIFI_STORM_LATCHED=true
  [ $((now - WIFI_LAST_ATTEMPT)) -ge 600 ] || return 1
  WIFI_LAST_ATTEMPT=$now
  WIFI_WATCHDOG_REASON=$kind
  return 0
}

function wifi_watchdog_uptime {
  local seconds rest
  read -r seconds rest < /proc/uptime
  printf '%s\n' "${seconds%%.*}"
}

function wifi_watchdog_sanitize {
  sed -E '/[Ss][Ss][Ii][Dd]:/d; s/([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}/[MAC]/g; s/([0-9]{1,3}\.){3}[0-9]{1,3}/[IP]/g; s/([[:xdigit:]]{1,4}:){3,}[[:xdigit:]:%]+/[IP]/g; s/[[:xdigit:]:]*::[[:xdigit:]:]+(%[[:alnum:]_.-]+)?/[IP]/g; s/server [^ ]+ not responding/server [host] not responding/g'
}

function wifi_watchdog_capture {
  local destination=${WIFI_WATCHDOG_DIAGNOSTICS:-/mutable/wifi-health-latest.txt}
  (
    umask 077
    {
      printf 'Wi-Fi watchdog pre-recovery: %s\nReason: %s\n' "$(date -Is)" "$WIFI_WATCHDOG_REASON"
      printf '\nKernel transport events\n'
      timeout -k 1 3 dmesg 2>/dev/null | grep -E 'brcmfmac|mmc1|nfs: server|under-voltage|throttl' | tail -n 80 || true
      printf '\nWi-Fi link\n'
      timeout -k 1 3 iw dev wlan0 link || true
      timeout -k 1 3 iw dev wlan0 station dump || true
      timeout -k 1 2 iw dev wlan0 get power_save || true
      printf '\nInterface counters\n'
      timeout -k 1 2 ip -s link show dev wlan0 || true
      printf '\nNetworkManager state\n'
      timeout -k 1 2 nmcli -f GENERAL.STATE,GENERAL.REASON device show wlan0 || true
      printf '\nLoad and power\n'
      cat /proc/loadavg /proc/pressure/io 2>/dev/null || true
      timeout -k 1 2 vcgencmd get_throttled || true
    } 2>&1 | wifi_watchdog_sanitize | head -c 32768 > "${destination}.tmp" &&
      mv -f "${destination}.tmp" "$destination"
  ) || log 'Wi-Fi watchdog: could not save pre-recovery diagnostics'
}

function wifi_watchdog_reload {
  local unload_result=0 load_result=0
  # Leave shared cfg80211/brcmutil dependencies and other adapters alone.
  timeout -k 2 8 modprobe -r brcmfmac >/dev/null 2>&1 || unload_result=$?
  timeout -k 2 8 modprobe brcmfmac >/dev/null 2>&1 || load_result=$?
  if [ "$unload_result" -ne 0 ] || [ "$load_result" -ne 0 ]; then
    log "Wi-Fi watchdog: recovery failed (unload=$unload_result, load=$load_result); no further attempt for this storm"
    return 1
  fi
}

function wifi_watchdog_verify_link {
  local deadline now gateway
  deadline=$(( $(wifi_watchdog_uptime) + 30 ))
  while true; do
    now=$(wifi_watchdog_uptime)
    [ "$now" -lt "$deadline" ] || return 1
    if timeout -k 1 2 iw dev wlan0 link 2>/dev/null | grep -q 'Connected to'; then
      gateway=$(timeout -k 1 2 ip -4 route show default dev wlan0 2>/dev/null | awk '$1 == "default" && $2 == "via" {print $3; exit}') || true
      if [ -n "$gateway" ] && timeout -k 1 2 ping -I wlan0 -c 1 -W 1 "$gateway" >/dev/null 2>&1; then
        return 0
      fi
    fi
    sleep 2
  done
}

function wifi_watchdog_recover {
  wifi_watchdog_capture
  log "Wi-Fi watchdog: attempting driver reload after $WIFI_WATCHDOG_REASON error burst"
  if ! wifi_watchdog_reload; then return 1; fi
  if wifi_watchdog_verify_link; then
    log 'Wi-Fi watchdog: reload completed, associated and gateway reachable; throughput and archive recovery unverified'
  else
    log 'Wi-Fi watchdog: reload completed but connection could not be verified; check diagnostics; no further attempt for this storm'
    return 1
  fi
}

function wifi_watchdog_attempt {
  local runtime=${WIFI_WATCHDOG_RUNTIME_DIR:-/run}
  (
    umask 077
    flock -n 9 || { log 'Wi-Fi watchdog: another recovery is in progress'; exit 0; }
    local now last=-600 updated
    now=$(wifi_watchdog_uptime)
    if [ -r "$runtime/dashusb_wifi_watchdog.attempt" ]; then
      read -r last < "$runtime/dashusb_wifi_watchdog.attempt" || true
      [[ $last =~ ^[0-9]+$ ]] || last=-600
    fi
    [ $((now - last)) -ge 600 ] || exit 0
    if grep -q '"state"[[:space:]]*:[[:space:]]*"running"' /mutable/wifi-firmware/state.json 2>/dev/null; then
      updated=$(stat -c %Y /mutable/wifi-firmware/state.json 2>/dev/null) || updated=0
      if [ $(( $(date +%s) - updated )) -lt 900 ]; then
        log 'Wi-Fi watchdog: recovery skipped during firmware operation'
        exit 0
      fi
    fi
    printf '%s\n' "$now" > "$runtime/dashusb_wifi_watchdog.attempt" || { log 'Wi-Fi watchdog: could not record cooldown; recovery skipped'; exit 1; }
    wifi_watchdog_recover
  ) 9>"$runtime/dashusb_wifi_watchdog.lock"
}

function wifichecker {
  echo -n wifichecker > /proc/self/comm
  wifi_watchdog_init
  # Only new kernel messages: a service restart must not replay an old storm.
  dmesg -W | {
    local line event_time
    while read -r line; do
      if [[ $line =~ ^\[[[:space:]]*([0-9]+)\. ]]; then
        event_time=${BASH_REMATCH[1]}
      else
        event_time=$(wifi_watchdog_uptime)
      fi
      if wifi_watchdog_observe "$line" "$event_time"; then
        wifi_watchdog_attempt || true
      fi
    done
    log 'Wi-Fi watchdog: kernel event stream ended; monitoring stopped'
  }
}
