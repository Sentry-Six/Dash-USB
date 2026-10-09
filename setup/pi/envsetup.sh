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
    # setup_file resolves only from the fixed paths above.
    # shellcheck disable=SC1090
    safesource $setup_file
  else
    echo "couldn't find $setup_file"
    return 1
  fi

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

  # Defaults only for scripts that source this environment.
  export DASHUSB_HOSTNAME=${DASHUSB_HOSTNAME:-dashusb}
  export NOTIFICATION_TITLE=${NOTIFICATION_TITLE:-${DASHUSB_HOSTNAME}}
  export RTC_BATTERY_ENABLED=${RTC_BATTERY_ENABLED:-false}
}

read_setup_variables

# Mobile push credentials use the daemon-managed JSON store, not shell config.
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
fi

function isPi5 {
  grep -q "Raspberry Pi 5" /sys/firmware/devicetree/base/model
}
export -f isPi5

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
  # An existing loop prevents identifying any side effect of the failed call.
    return 1
  fi
  local newloop=$(losetup -n -O NAME -j "$lastarg")
  if [ -z "$newloop" ]
  then
    # No loop device was created.
    return 1
  fi
  echo "$newloop"
}

export -f losetup_find_show
