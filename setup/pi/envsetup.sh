#!/bin/bash -eu

if [ "${BASH_SOURCE[0]}" = "$0" ]
then
  echo "$0 must be sourced, not executed"
  exit 1
fi

if [ ! -L /dashusb ]
then
  mount / -o remount,rw
  rm -rf /dashusb
  if [ -d /boot/firmware ] && findmnt --fstab /boot/firmware &> /dev/null
  then
    ln -s /boot/firmware /dashusb
  else
    ln -s /boot /dashusb
  fi
fi

function safesource {
  cat <<EOF > /tmp/checksetupconf
#!/bin/bash -eu
source '$1' &> /tmp/checksetupconf.out
EOF
  chmod +x /tmp/checksetupconf
  if ! /tmp/checksetupconf
  then
    if declare -F setup_progress > /dev/null
    then
      setup_progress "Error in $1:"
      setup_progress "$(cat /tmp/checksetupconf.out)"
    else
      echo "Error in $1:"
      cat /tmp/checksetupconf.out
    fi
    exit 1
  fi
  # shellcheck disable=SC1090
  source "$1"
}

function read_setup_variables {
  if [ -z "${setup_file+x}" ]
  then
    local -r setup_file=/root/dashusb.conf
  fi
  if [ -e $setup_file ]
  then
    # setup_file is effectively a constant, which shellcheck can't see.
    # shellcheck disable=SC1090
    safesource $setup_file
  else
    echo "couldn't find $setup_file"
    return 1
  fi

  # TODO: change this "declare" to "local" when github updates
  # to a newer shellcheck.
  declare -A newnamefor

  newnamefor[archiveserver]=ARCHIVE_SERVER
  newnamefor[camsize]=CAM_SIZE
  newnamefor[sharename]=SHARE_NAME
  newnamefor[shareuser]=SHARE_USER
  newnamefor[sharepassword]=SHARE_PASSWORD
  newnamefor[timezone]=TIME_ZONE
  newnamefor[usb_drive]=DATA_DRIVE
  newnamefor[USB_DRIVE]=DATA_DRIVE
  newnamefor[archivedelay]=ARCHIVE_DELAY
  newnamefor[trigger_file_any]=TRIGGER_FILE_ANY
  newnamefor[pushover_enabled]=PUSHOVER_ENABLED
  newnamefor[pushover_user_key]=PUSHOVER_USER_KEY
  newnamefor[pushover_app_key]=PUSHOVER_APP_KEY
  newnamefor[gotify_enabled]=GOTIFY_ENABLED
  newnamefor[gotify_domain]=GOTIFY_DOMAIN
  newnamefor[gotify_app_token]=GOTIFY_APP_TOKEN
  newnamefor[gotify_priority]=GOTIFY_PRIORITY
  newnamefor[ifttt_enabled]=IFTTT_ENABLED
  newnamefor[ifttt_event_name]=IFTTT_EVENT_NAME
  newnamefor[ifttt_key]=IFTTT_KEY
  newnamefor[sns_enabled]=SNS_ENABLED
  newnamefor[aws_region]=AWS_REGION
  newnamefor[aws_access_key_id]=AWS_ACCESS_KEY_ID
  newnamefor[aws_secret_key]=AWS_SECRET_ACCESS_KEY
  newnamefor[aws_sns_topic_arn]=AWS_SNS_TOPIC_ARN

  local oldname
  for oldname in "${!newnamefor[@]}"
  do
    local newname=${newnamefor[$oldname]}
    if [[ -z ${!newname+x} ]] && [[ -n ${!oldname+x} ]]
    then
      local value=${!oldname}
      export $newname="$value"
      unset $oldname
    fi
  done

  # Defaults for anything the config didn't set.
  REPO=${REPO:-Sentry-Six}
  REPO_NAME=${REPO_NAME:-Dash-USB}
  SNAPSHOTS_ENABLED=${SNAPSHOTS_ENABLED:-true}
  if [ "$SNAPSHOTS_ENABLED" != "true" ]
  then
    BRANCH="no-snapshots"
    if declare -F setup_progress > /dev/null
    then
      setup_progress "WARNING: using '$BRANCH' branch because SNAPSHOTS_ENABLED is not true"
    else
      echo "WARNING: using '$BRANCH' branch because SNAPSHOTS_ENABLED is not true"
    fi
  else
    BRANCH=${BRANCH:-main}
  fi
  CONFIGURE_ARCHIVING=${CONFIGURE_ARCHIVING:-true}
  UPGRADE_PACKAGES=${UPGRADE_PACKAGES:-false}
  export DASHUSB_HOSTNAME=${DASHUSB_HOSTNAME:-dashusb}
  export NOTIFICATION_TITLE=${NOTIFICATION_TITLE:-${DASHUSB_HOSTNAME}}
  SAMBA_ENABLED=${SAMBA_ENABLED:-false}
  SAMBA_GUEST=${SAMBA_GUEST:-false}
  INCREASE_ROOT_SIZE=${INCREASE_ROOT_SIZE:-0}
  export CAM_SIZE=${CAM_SIZE:-0}
  export DATA_DRIVE=${DATA_DRIVE:-''}
  export RTC_BATTERY_ENABLED=${RTC_BATTERY_ENABLED:-false}
  export RTC_TRICKLE_CHARGE=${RTC_TRICKLE_CHARGE:-false}
}

read_setup_variables

# Mobile push credentials are deliberately NOT stored in dashusb.conf. The
# daemon-managed JSON file below is the single source of truth, which keeps
# concurrent conf writes from racing.
NOTIFICATION_CREDENTIALS_JSON="/root/.dashusb/notification-credentials.json"
if [ "${MOBILE_PUSH_ENABLED:-false}" = "true" ] && [ -f "$NOTIFICATION_CREDENTIALS_JSON" ]; then
  MOBILE_PUSH_DEVICE_ID=$(sed -n 's/.*"device_id" *: *"\([^"]*\)".*/\1/p' "$NOTIFICATION_CREDENTIALS_JSON")
  MOBILE_PUSH_SECRET=$(sed -n 's/.*"device_secret" *: *"\([^"]*\)".*/\1/p' "$NOTIFICATION_CREDENTIALS_JSON")
  export MOBILE_PUSH_DEVICE_ID MOBILE_PUSH_SECRET
fi

if [ -t 0 ]
then
  if ! declare -F log > /dev/null 
  then
    function log { echo "$@"; }
    export -f log
  fi
  complete -W "diagnose upgrade install" setup-dashusb
fi

function isRaspberryPi {
  grep -q "Raspberry Pi" /sys/firmware/devicetree/base/model
}

function isPi5 {
  grep -q "Raspberry Pi 5" /sys/firmware/devicetree/base/model
}
export -f isPi5

function isPi4 {
  grep -q "Raspberry Pi 4" /sys/firmware/devicetree/base/model
}
export -f isPi4

function isPi2 {
  grep -q "Raspberry Pi Zero 2" /sys/firmware/devicetree/base/model
}
export -f isPi2

function isPi3 {
  grep -q "Raspberry Pi 3" /sys/firmware/devicetree/base/model
}
export -f isPi3

function isRockPi4 {
  grep -q "ROCK Pi 4" /sys/firmware/devicetree/base/model
}
export -f isRockPi4

function isRadxaZero {
  grep -q "Radxa Zero" /sys/firmware/devicetree/base/model
}
export -f isRadxaZero

STATUSLED=/tmp/fakeled

while read -r led
do
  case "$led" in
    *status | */led0 | */ACT | */user-led2 | */radxa-zero:green)
      STATUSLED="$led"
      break;
      ;;
    *)
      ;;
    esac
done < <(find /sys/class/leds -type l)

if [ ! -d "$STATUSLED" ]
then
  mkdir -p "$STATUSLED"
fi

if [ -f /dashusb/cmdline.txt ]
then
  export CMDLINE_PATH=/dashusb/cmdline.txt
else
  export CMDLINE_PATH=/dev/null
fi

if [ -f /dashusb/config.txt ]
then
  export PICONFIG_PATH=/dashusb/config.txt
else
  export PICONFIG_PATH=/dev/null
fi

# losetup can fail on a kernel/userland mismatch
# (https://lore.kernel.org/lkml/8bed44f2-273c-856e-0018-69f127ea4258@linux.ibm.com/)
# yet still create the loop device, so recheck before reporting failure.
function losetup_find_show {
  local lastarg="${@:$#}"
  local loop=$(losetup -n -O NAME -j "$lastarg")
  if losetup -f --show "$@"
  then
    return
  fi
  if [ -n "$loop" ]
  then
    # losetup failed and the file already had a loop device, so a new one
    # can't be identified. Report failure rather than guess.
    return 1
  fi
  local newloop=$(losetup -n -O NAME -j "$lastarg")
  if [ -z "$newloop" ]
  then
    # losetup truly failed: no loop device was created
    return 1
  fi
  echo "$newloop"
}

export -f losetup_find_show
