#!/bin/bash -eu

function log_progress () {
  if declare -F setup_progress > /dev/null
  then
    setup_progress "create-backingfiles-partition: $1"
    return
  fi
  echo "create-backingfiles-partition: $1"
}

if ! hash mkfs.xfs
then
  apt-get -y install xfsprogs
fi

function partition_prefix_for {
  case $1 in
    /dev/mmcblk* | /dev/nvme* | /dev/loop*)
      echo p
      ;;
    /dev/sd*)
      echo
      ;;
    *)
      log_progress "STOP: can't determine partition naming scheme for '$1'"
      exit 1
      ;;
  esac
}

BACKINGFILES_MOUNTPOINT="${1:-none}"
MUTABLE_MOUNTPOINT="${2:-none}"
function update_fstab {
  if grep -q "LABEL=backingfiles" /etc/fstab
  then
    # nofail on the existing entry: without it a missing drive hangs boot.
    if ! grep "LABEL=backingfiles" /etc/fstab | grep -q "nofail"
    then
      log_progress "Adding nofail to existing backingfiles fstab entry"
      sed -i '/LABEL=backingfiles/ s/auto,rw/auto,rw,nofail/' /etc/fstab
    fi
    log_progress "backingfiles already defined in /etc/fstab. Not modifying /etc/fstab."
  elif [ "$BACKINGFILES_MOUNTPOINT" != "none" ]
  then
    echo "LABEL=backingfiles $BACKINGFILES_MOUNTPOINT xfs auto,rw,noatime,nofail 0 2" >> /etc/fstab
  fi
  if grep -q 'LABEL=mutable' /etc/fstab
  then
    if ! grep "LABEL=mutable" /etc/fstab | grep -q "nofail"
    then
      log_progress "Adding nofail to existing mutable fstab entry"
      sed -i '/LABEL=mutable/ s/auto,rw/auto,rw,nofail/' /etc/fstab
    fi
    log_progress "mutable already defined in /etc/fstab. Not modifying /etc/fstab."
  elif [ "$MUTABLE_MOUNTPOINT" != "none" ]
  then
    echo "LABEL=mutable $MUTABLE_MOUNTPOINT ext4 auto,rw,nofail 0 2" >> /etc/fstab
  fi
}

# An external data drive takes precedence over the SD card.
if [ -n "$DATA_DRIVE" ]
then
  log_progress "DATA_DRIVE is set to $DATA_DRIVE"
  PARTITION_PREFIX=$(partition_prefix_for "$DATA_DRIVE")
  P1="${DATA_DRIVE}${PARTITION_PREFIX}1"
  P2="${DATA_DRIVE}${PARTITION_PREFIX}2"
  # Reuse existing partitions when the labels already match. A config-only
  # wizard re-run (ARCHIVE_SERVER, etc.) must NEVER wipe data just because
  # blkid was momentarily slow or the FS was remounting. fstab is rewritten
  # below either way, keep or wipe.
  if [ /dev/disk/by-label/backingfiles -ef "$P2" ] && \
     [ /dev/disk/by-label/mutable -ef "$P1" ]
  then
    log_progress "Existing backingfiles (xfs) and mutable (ext4) partitions found on $DATA_DRIVE. Keeping them."
    # Quiesce anything holding the partitions open so the next mount finds
    # them clean. On this keep-existing path there must be NO mkfs and NO
    # xfs_repair; never reformat. Mount replays the XFS log safely (slow on
    # 1TB+ drives, but that is a UX cost, not a data one), and a genuinely
    # broken log fails the mount loudly so the user can run xfs_repair.
    killall archiveloop 2>/dev/null || true
    /root/bin/disable_gadget.sh 2>/dev/null || true
    for loop in $(losetup -a 2>/dev/null | grep -E '/backingfiles/|/mnt/' | cut -d: -f1); do
      umount "$loop" 2>/dev/null || true
      losetup -d "$loop" 2>/dev/null || true
    done
    for mp in /mnt/cam /mnt/music /mnt/lightshow /mnt/boombox /backingfiles /mutable; do
      umount "$mp" 2>/dev/null || true
    done
    umount "$P1" 2>/dev/null || true
    umount "$P2" 2>/dev/null || true
    sleep 2
  else
    # Unmount any partitions on the data drive before wiping, otherwise
    # wipefs/parted/mkfs will hang waiting for exclusive device access.
    log_progress "Unmounting partitions on $DATA_DRIVE..."
    killall archiveloop 2>/dev/null || true
    /root/bin/disable_gadget.sh 2>/dev/null || true
    # Detach loop devices backed by files on the data drive partitions.
    # Pre-existing backing images (cam_disk.bin and friends) stay
    # loop-mounted and block unmount/wipefs until they are detached.
    for loop in $(losetup -a 2>/dev/null | grep -E '/backingfiles/|/mnt/' | cut -d: -f1); do
      umount "$loop" 2>/dev/null || true
      losetup -d "$loop" 2>/dev/null || true
    done
    for mp in /mnt/cam /mnt/music /mnt/lightshow /mnt/boombox /backingfiles /mutable; do
      umount "$mp" 2>/dev/null || true
    done
    # Also unmount by device in case the mount points differ
    for part in "${P1}" "${P2}"; do
      umount "$part" 2>/dev/null || true
    done
    # Give the kernel time to release device handles, which takes longer on
    # large drives that had many open files.
    sleep 3

    log_progress "WARNING !!! This will delete EVERYTHING in $DATA_DRIVE."
    wipefs -afq "$DATA_DRIVE"
    parted "$DATA_DRIVE" --script mktable gpt
    log_progress "$DATA_DRIVE fully erased. Creating partitions..."
    parted -a optimal -m "$DATA_DRIVE" mkpart primary ext4 '0%' 2GB
    parted -a optimal -m "$DATA_DRIVE" mkpart primary ext4 2GB '100%'
    udevadm settle --timeout=10 2>/dev/null || sleep 2
    log_progress "Backing files and mutable partitions created."

    log_progress "Formatting new partitions..."
    log_progress "Formatting mutable partition (ext4) on $P1..."
    mkfs.ext4 -F -L mutable "$P1"
    log_progress "Formatting backingfiles partition (xfs) on $P2..."
    mkfs.xfs -f -K -m reflink=1 -L backingfiles "$P2"
    log_progress "Partition formatting complete."
  fi

  update_fstab
  log_progress "Done."
  exit 0
else
  echo "DATA_DRIVE not set. Proceeding to SD card setup"
fi

LAST_PARTITION_DEVICE=$(sfdisk -q -l "$BOOT_DISK" | tail -1 | awk '{print $1}')
readonly LAST_PARTITION_DEVICE
LAST_PART_NUM=$(echo "$LAST_PARTITION_DEVICE" | grep -o '[0-9]*$')
readonly LAST_PART_NUM
readonly SECOND_TO_LAST_PART_NUM=$((LAST_PART_NUM - 1))
LAST_PARTITION_DEVICE_PREFIX=$(echo "$LAST_PARTITION_DEVICE" | sed 's/[0-9]*$//')
readonly LAST_PARTITION_DEVICE_PREFIX
readonly SECOND_TO_LAST_PARTITION_DEVICE=${LAST_PARTITION_DEVICE_PREFIX}${SECOND_TO_LAST_PART_NUM}
if [ /dev/disk/by-label/mutable -ef "$LAST_PARTITION_DEVICE" ]
then
  readonly MUTABLE_DEVICE="$LAST_PARTITION_DEVICE"
else
  readonly MUTABLE_DEVICE="${BOOT_DEVICE_PARTITION_PREFIX}$((LAST_PART_NUM + 2))"
fi
if [ /dev/disk/by-label/backingfiles -ef "$SECOND_TO_LAST_PARTITION_DEVICE" ]
then
  readonly BACKINGFILES_DEVICE="$SECOND_TO_LAST_PARTITION_DEVICE"
else
  readonly BACKINGFILES_DEVICE="${BOOT_DEVICE_PARTITION_PREFIX}$((LAST_PART_NUM + 1))"
fi

# Correct layout already on disk (backingfiles then mutable): keep it. xfs
# needs no work; an ext4 backingfiles gets converted below.
if [ /dev/disk/by-label/backingfiles -ef "${BACKINGFILES_DEVICE}" ] && \
    [ /dev/disk/by-label/mutable -ef "${MUTABLE_DEVICE}" ] && \
    blkid "${MUTABLE_DEVICE}" | grep -q 'TYPE="ext4"'
then
  if blkid "${BACKINGFILES_DEVICE}" | grep -q 'TYPE="xfs"'
  then
    # Created by an earlier setup run or by the user; assume big enough.
    log_progress "using existing backingfiles and mutable partitions"
    update_fstab
    return &> /dev/null || exit 0
  elif blkid "${BACKINGFILES_DEVICE}" | grep -q 'TYPE="ext4"'
  then
    # Convert an existing ext4 backingfiles partition to xfs (reflink).
    log_progress "reformatting existing backingfiles as xfs"
    killall archiveloop || true
    /root/bin/disable_gadget.sh || true
    if mount | grep -qw "/mnt/cam"
    then
      if ! umount /mnt/cam
      then
        log_progress "STOP: couldn't unmount /mnt/cam"
        exit 1
      fi
    fi
    if mount | grep -qw "/backingfiles"
    then
      if ! umount /backingfiles
      then
        log_progress "STOP: couldn't unmount /backingfiles"
        exit 1
      fi
    fi
    mkfs.xfs -f -K -m reflink=1 -L backingfiles "${BACKINGFILES_DEVICE}"

    sed -i 's/LABEL=backingfiles .*/LABEL=backingfiles \/backingfiles xfs auto,rw,noatime 0 2/' /etc/fstab
    mount /backingfiles
    log_progress "backingfiles converted to xfs and mounted"
    return &> /dev/null || exit 0
  fi
fi

# backingfiles and mutable partitions either don't exist, or are the wrong type
if [ -e "${BACKINGFILES_DEVICE}" ] || [ -e "${MUTABLE_DEVICE}" ]
then
  log_progress "STOP: partitions already exist, but are not as expected"
  log_progress "please delete them and re-run setup"
  exit 1
fi

log_progress "Checking existing partitions..."

DISK_SECTORS=$(blockdev --getsz "${BOOT_DISK}")
LAST_DISK_SECTOR=$((DISK_SECTORS - 1))
# mutable takes the last 300MB of the disk
FIRST_MUTABLE_SECTOR=$((LAST_DISK_SECTOR-614400+1))
# backingfiles fills the gap between the last existing partition and mutable
LAST_PART_SECTOR=$(sfdisk -o End -q -l "${BOOT_DISK}" | tail +2 | sort -n | tail -1)
FIRST_BACKINGFILES_SECTOR=$((LAST_PART_SECTOR + 1))
# Round up to a 1MB boundary: some prebuilt and older Armbian images have an
# odd root partition size.
FIRST_BACKINGFILES_SECTOR=$(((FIRST_BACKINGFILES_SECTOR + 2047) / 2048 * 2048))
BACKINGFILES_NUM_SECTORS=$((FIRST_MUTABLE_SECTOR - FIRST_BACKINGFILES_SECTOR))

# /mutable needs one inode per symlink to a recording. One gigabyte of
# /backingfiles holds roughly 36 recording files; with headroom for short
# recordings and directories that works out to about 1 inode per 20000
# sectors of /backingfiles.
NUM_MUTABLE_INODES=$((BACKINGFILES_NUM_SECTORS / 20000))

ORIGINAL_DISK_IDENTIFIER=$( fdisk -l "${BOOT_DISK}" | grep -e "^Disk identifier" | sed "s/Disk identifier: 0x//" )

log_progress "Modifying partition table for backing files partition..."
echo "$FIRST_BACKINGFILES_SECTOR,$BACKINGFILES_NUM_SECTORS" | sfdisk --force --no-reread "${BOOT_DISK}" -N $((LAST_PART_NUM + 1))

log_progress "Modifying partition table for mutable (writable) partition for script usage..."
echo "$FIRST_MUTABLE_SECTOR," | sfdisk --force --no-reread "${BOOT_DISK}" -N $((LAST_PART_NUM + 2))

partprobe "${BOOT_DISK}" 2>/dev/null || true
udevadm settle --timeout=10 2>/dev/null || sleep 2

# partprobe doesn't always take; add the partitions to the kernel's view.
if [ ! -e "${BACKINGFILES_DEVICE}" ] || [ ! -e "${MUTABLE_DEVICE}" ]
then
  partx --add --nr $((LAST_PART_NUM + 1)):$((LAST_PART_NUM + 2)) "${BOOT_DISK}"
  udevadm settle --timeout=10 2>/dev/null || sleep 2
fi
if [ ! -e "${BACKINGFILES_DEVICE}" ] || [ ! -e "${MUTABLE_DEVICE}" ]
then
  log_progress "failed to add partitions"
  exit 1
fi

NEW_DISK_IDENTIFIER=$( fdisk -l "${BOOT_DISK}" | grep -e "^Disk identifier" | sed "s/Disk identifier: 0x//" )

log_progress "Writing updated partitions to fstab and cmdline.txt"
sed -i "s/${ORIGINAL_DISK_IDENTIFIER}/${NEW_DISK_IDENTIFIER}/g" /etc/fstab
if [ -f "$CMDLINE_PATH" ]
then
  sed -i "s/${ORIGINAL_DISK_IDENTIFIER}/${NEW_DISK_IDENTIFIER}/" "$CMDLINE_PATH"
fi

log_progress "Formatting new partitions..."
log_progress "Formatting backingfiles partition (xfs) on ${BACKINGFILES_DEVICE}..."
mkfs.xfs -f -K -m reflink=1 -L backingfiles "${BACKINGFILES_DEVICE}"
log_progress "Formatting mutable partition (ext4) on ${MUTABLE_DEVICE}..."
mkfs.ext4 -F -N "$NUM_MUTABLE_INODES" -L mutable "${MUTABLE_DEVICE}"
log_progress "Partition formatting complete."

update_fstab
