#!/bin/bash

# Adapted from https://github.com/adafruit/Raspberry-Pi-Installer-Scripts/blob/master/read-only-fs.sh

function log_progress () {
  if declare -F setup_progress > /dev/null
  then
    setup_progress "make-root-fs-readonly: $1"
    return
  fi
  echo "make-root-fs-readonly: $1"
}

if [ "${SKIP_READONLY:-false}" = "true" ]
then
  log_progress "Skipping"
  exit 0
fi

log_progress "start"

# The boot/firmware partition must be writable: on an upgrade, an older
# remountfs_rw may have remounted only root, leaving the partition that holds
# cmdline.txt read-only, and the sed -i on CMDLINE_PATH below would fail.
for _mp in /dashusb /teslausb /boot/firmware /boot; do
  if findmnt "$_mp" > /dev/null 2>&1; then
    mount "$_mp" -o remount,rw 2>/dev/null || true
    break
  fi
done

function append_cmdline_txt_param() {
  local toAppend="$1"
  # Never add an option twice: an over-long kernel command line stops the Pi
  # from booting. Match the option at the end ($) or mid-line surrounded by
  # whitespace (\s).
  if [ -f "$CMDLINE_PATH" ] && ! grep -P -q "\s${toAppend}(\$|\s)" "$CMDLINE_PATH"
  then
    sed -i "s/\'/ ${toAppend}/g" "$CMDLINE_PATH" >/dev/null
  fi
}

function remove_cmdline_txt_param() {
  if [ -f "$CMDLINE_PATH" ]
  then
    sed -i "s/\(\s\)${1}\(\s\|$\)//" "$CMDLINE_PATH" > /dev/null
  fi
}

log_progress "Disabling unnecessary service..."
systemctl disable apt-daily.timer
systemctl disable apt-daily-upgrade.timer

# The adb service on some distributions interferes with mass storage emulation.
systemctl disable amlogic-adbd &> /dev/null || true
systemctl disable radxa-adbd radxa-usbnet &> /dev/null || true

# Don't restore the LED state captured when the root fs was made read-only.
systemctl disable armbian-led-state &> /dev/null || true

log_progress "Removing unwanted packages..."
# Protect NetworkManager and the WiFi packages from autoremove. On non-Raspbian
# distros (e.g. DietPi) they may be auto-installed dependencies, and autoremove
# would purge them and kill WiFi.
for pkg in network-manager wpasupplicant wpa-supplicant ifupdown dhcpcd dhcpcd5 isc-dhcp-client firmware-brcm80211 firmware-realtek firmware-atheros firmware-iwlwifi firmware-misc-nonfree
do
  if dpkg -s "$pkg" &> /dev/null
  then
    apt-mark manual "$pkg" 2>/dev/null || true
  fi
done
apt-get remove -y --purge triggerhappy logrotate dphys-swapfile
apt-get -y autoremove --purge
# Replace rsyslog with busybox-syslogd; read the log with logread.
log_progress "Installing ntp and busybox-syslogd..."
apt-get -y install ntp busybox-syslogd; dpkg --purge rsyslog

log_progress "Configuring system..."

# fastboot suppresses fsck, so drop it before requesting fsck.mode=auto.
remove_cmdline_txt_param fastboot
append_cmdline_txt_param fsck.mode=auto
append_cmdline_txt_param noswap
append_cmdline_txt_param ro

# Max mount count of 1 means root and mutable are checked on every boot.
tune2fs -c 1 "$ROOT_PARTITION_DEVICE" || log_progress "tune2fs failed for rootfs"
tune2fs -c 1 /dev/disk/by-label/mutable || log_progress "tune2fs failed for mutable"

# Swap is disabled (noswap above), so reclaim the swap file's space.
rm -f /var/swap

# Move fake-hwclock.data to /mutable so it stays writable. Do this even when
# RTC_BATTERY_ENABLED=true: configure-rtc.sh (which installs the hwclock
# service and disables fake-hwclock) runs later, so a reboot between the two
# would otherwise have no time source at all.
if ! findmnt --mountpoint /mutable > /dev/null
then
  log_progress "Mounting the mutable partition..."
  mount /mutable
  log_progress "Mounted."
fi
if [ ! -e "/mutable/etc" ]
then
  mkdir -p /mutable/etc
fi

if [ ! -L "/etc/fake-hwclock.data" ] && [ -e "/etc/fake-hwclock.data" ]
then
  log_progress "Moving fake-hwclock data"
  mv /etc/fake-hwclock.data /mutable/etc/fake-hwclock.data
  ln -s /mutable/etc/fake-hwclock.data /etc/fake-hwclock.data
fi
# fake-hwclock runs in early boot by default, before /mutable is mounted, and
# fails. Delay it until after mutable.mount.
if [ -e /lib/systemd/system/fake-hwclock.service ]
then
  sed -i 's/Before=.*/After=mutable.mount/' /lib/systemd/system/fake-hwclock.service
fi

# ---- NetworkManager runtime state (/var/lib/NetworkManager) ----
# Use a tmpfs mount, never a symlink to /mutable. NM's built-in dnsmasq writes
# lease files here (e.g. dnsmasq-ap0.leases); if the directory isn't writable,
# the AP connection enters an enable/disable loop that thrashes the WiFi
# hardware and kills all wireless connectivity. A tmpfs is always writable and
# doesn't wait on the USB drive mounting in time.
if [ -d /var/lib/NetworkManager/ ] && [ ! -L /var/lib/NetworkManager ]
then
  log_progress "Backing up /var/lib/NetworkManager to mutable"
  mkdir -p /mutable/var/lib/
  cp -a /var/lib/NetworkManager /mutable/var/lib/ 2>/dev/null || true
fi
# Undo symlink left by a previous setup so the tmpfs mount works
if [ -L /var/lib/NetworkManager ]
then
  log_progress "Replacing /var/lib/NetworkManager symlink with directory for tmpfs"
  rm /var/lib/NetworkManager
  mkdir -p /var/lib/NetworkManager
fi

# ---- NetworkManager connection profiles ----
# Keep profiles on the root FS so they are available at boot even before
# /mutable (which may live on a USB drive) mounts. A backup copy goes to
# /mutable for reference and future restores.
if [ -d /etc/NetworkManager/system-connections ] && [ ! -L /etc/NetworkManager/system-connections ]
then
  log_progress "Backing up NetworkManager connection profiles to mutable"
  mkdir -p /mutable/etc/NetworkManager
  cp -a /etc/NetworkManager/system-connections /mutable/etc/NetworkManager/
fi
# Undo a symlink left by a previous setup: restore the real directory from the
# mutable backup so NM finds the profiles on root at boot.
if [ -L /etc/NetworkManager/system-connections ]
then
  log_progress "Restoring NetworkManager connection profiles to root FS"
  rm /etc/NetworkManager/system-connections
  if [ -d /mutable/etc/NetworkManager/system-connections ]
  then
    cp -a /mutable/etc/NetworkManager/system-connections /etc/NetworkManager/
  else
    mkdir -p /etc/NetworkManager/system-connections
  fi
fi

# ---- DHCP lease directories ----
# Use tmpfs mounts; leases are ephemeral and re-requested at boot. A symlink to
# /mutable fails when the USB drive isn't mounted in time: DHCP clients can't
# write leases, so the device gets no IP address.
if [ -L /var/lib/dhcp ]
then
  log_progress "Replacing /var/lib/dhcp symlink with directory for tmpfs"
  rm /var/lib/dhcp
  mkdir -p /var/lib/dhcp
fi
if [ -L /var/lib/dhcpcd ]
then
  log_progress "Replacing /var/lib/dhcpcd symlink with directory for tmpfs"
  rm /var/lib/dhcpcd
  mkdir -p /var/lib/dhcpcd
fi

if [ ! -e "/mutable/configs" ]
then
  mkdir -p /mutable/configs
fi

# /var/spool must be a real directory for the tmpfs entry added below.
if [ -L /var/spool ]
then
  log_progress "fixing /var/spool"
  rm /var/spool
  mkdir /var/spool
  chmod 755 /var/spool
else
  rm -rf /var/spool/*
fi

# /var/spool on tmpfs needs mode 1777, not the 0755 var.conf ships.
sed -i "s/spool\s*0755/spool 1777/g" /usr/lib/tmpfiles.d/var.conf >/dev/null

# Point resolv.conf at /tmp, a tmpfs that is always writable at boot. Not
# /mutable: DNS breaks when the USB drive is slow to mount. Not
# systemd-resolved's stub (/run/systemd/resolve/...) either, because NM is
# configured with dns=none below and a dispatcher script populates
# resolv.conf, which systemd-resolved would fight.
# /tmp is wiped on every reboot, so the tmpfiles.d rule below MUST seed
# /tmp/resolv.conf at boot or the symlink dangles and DNS breaks.
_resolv_target=$(readlink -f /etc/resolv.conf 2>/dev/null || true)
if [ "$_resolv_target" != "/tmp/resolv.conf" ]
then
  log_progress "Redirecting resolv.conf to /tmp (was: ${_resolv_target:-empty})"
  # Seed with the current DHCP-provided DNS so name resolution survives the
  # rest of setup (e.g. apt-get upgrade). Sources in order: nmcli, the
  # existing resolv.conf, then a public fallback.
  > /tmp/resolv.conf
  if command -v nmcli &>/dev/null; then
    nmcli --terse --fields IP4.DNS dev show 2>/dev/null \
      | sed -n 's/^IP4\.DNS\[.*\]:/nameserver /p' \
      | head -3 >> /tmp/resolv.conf || true
  fi
  if ! grep -q '^nameserver' /tmp/resolv.conf 2>/dev/null; then
    # nmcli unavailable or returned nothing; try the existing resolv.conf
    [ -f "$_resolv_target" ] && grep '^nameserver' "$_resolv_target" >> /tmp/resolv.conf 2>/dev/null || true
  fi
  if ! grep -q '^nameserver' /tmp/resolv.conf 2>/dev/null; then
    echo "nameserver 1.1.1.1" >> /tmp/resolv.conf
  fi
  rm -f /etc/resolv.conf 2>/dev/null || true
  ln -sf /tmp/resolv.conf /etc/resolv.conf
fi

# Recreate /tmp/resolv.conf on every boot so the symlink never dangles.
# systemd-tmpfiles-setup.service runs after the tmpfs mounts but before the
# network stack, so the file exists in time. The public DNS fallback keeps
# early-boot resolution working; dhcpcd or NetworkManager overwrites it with
# the DHCP-provided servers (e.g. PiHole) once a lease arrives.
log_progress "Installing tmpfiles.d rule for resolv.conf"
mkdir -p /etc/tmpfiles.d
echo 'f /tmp/resolv.conf 0644 root root - nameserver 1.1.1.1' > /etc/tmpfiles.d/resolv-fallback.conf

# ---- DHCP client hooks to populate /tmp/resolv.conf ----
# On a read-only root, /etc/resolv.conf is a symlink to /tmp/resolv.conf.
# Install a hook for whichever DHCP client is present so DNS is populated when
# a lease arrives. Multiple hooks coexist harmlessly.

# -- NetworkManager: dns=none + dispatcher --
if command -v nmcli &>/dev/null
then
  log_progress "Configuring NetworkManager DNS handling (dns=none + dispatcher)"
  mkdir -p /etc/NetworkManager/conf.d
  cat > /etc/NetworkManager/conf.d/dashusb-dns.conf << 'EOF'
[main]
dns=none
EOF

  mkdir -p /etc/NetworkManager/dispatcher.d
  cat > /etc/NetworkManager/dispatcher.d/50-write-resolv-conf << 'DISPATCHER'
#!/bin/bash
# Populate /tmp/resolv.conf with DHCP-provided DNS servers.
case "$2" in
  up|dhcp4-change)
    _servers="${DHCP4_DOMAIN_NAME_SERVERS:-${IP4_NAMESERVERS:-}}"
    if [ -n "$_servers" ]; then
      {
        for _ns in $_servers; do
          echo "nameserver $_ns"
        done
        _domain="${DHCP4_DOMAIN_NAME:-}"
        [ -n "$_domain" ] && echo "search $_domain"
      } > /tmp/resolv.conf
    fi
    ;;
esac
DISPATCHER
  chmod 0755 /etc/NetworkManager/dispatcher.d/50-write-resolv-conf
fi

# -- dhcpcd: hook to write DHCP-provided DNS --
# DietPi and Raspberry Pi OS Lite use dhcpcd instead of NetworkManager.
if command -v dhcpcd &>/dev/null
then
  log_progress "Installing dhcpcd hook for resolv.conf"
  mkdir -p /lib/dhcpcd/dhcpcd-hooks
  cat > /lib/dhcpcd/dhcpcd-hooks/90-dashusb-resolv << 'DHCPHOOK'
# Write DHCP-provided DNS servers to /tmp/resolv.conf.
# /etc/resolv.conf is a symlink to /tmp/resolv.conf on DashUSB.
if [ -n "${new_domain_name_servers:-}" ]; then
  {
    for ns in $new_domain_name_servers; do
      echo "nameserver $ns"
    done
    [ -n "${new_domain_name:-}" ] && echo "search $new_domain_name"
  } > /tmp/resolv.conf
fi
DHCPHOOK
  chmod 0644 /lib/dhcpcd/dhcpcd-hooks/90-dashusb-resolv
fi

# -- ifupdown: hook for systems using /etc/network/interfaces + dhclient --
# dhclient normally writes /etc/resolv.conf directly (following the symlink).
# Install a hook as a safety net in case resolvconf intercepts that write.
if [ -d /etc/network ] && ! command -v nmcli &>/dev/null && ! command -v dhcpcd &>/dev/null
then
  log_progress "Installing ifupdown hook for resolv.conf"
  mkdir -p /etc/dhcp/dhclient-exit-hooks.d
  cat > /etc/dhcp/dhclient-exit-hooks.d/dashusb-resolv << 'DHCLIENTHOOK'
# Write DHCP-provided DNS to /tmp/resolv.conf (DashUSB read-only root).
if [ -n "${new_domain_name_servers:-}" ]; then
  {
    for ns in $new_domain_name_servers; do
      echo "nameserver $ns"
    done
    [ -n "${new_domain_name:-}" ] && echo "search $new_domain_name"
  } > /tmp/resolv.conf
fi
DHCLIENTHOOK
  chmod 0755 /etc/dhcp/dhclient-exit-hooks.d/dashusb-resolv
fi

# systemd-resolved conflicts with the resolv.conf handling above and is
# redundant once the dispatcher populates DNS directly.
if systemctl is-active --quiet systemd-resolved 2>/dev/null
then
  log_progress "Disabling systemd-resolved (dispatcher handles DNS directly)"
  systemctl stop systemd-resolved 2>/dev/null || true
  systemctl disable systemd-resolved 2>/dev/null || true
fi

# Ensure Bluetooth is not soft-blocked right now (for the rest of this setup).
rfkill unblock bluetooth 2>/dev/null || true

# Unblock Bluetooth at every boot. On Raspberry Pi the BT radio starts
# soft-blocked, and on a read-only root the block is never cleared, so BLE
# (and app pairing) never works. This oneshot runs before bluetooth.service
# and hciuart.service, so the radio is ready when bluetoothd starts.
log_progress "Installing Bluetooth rfkill-unblock boot service"
cat > /etc/systemd/system/rfkill-unblock-bluetooth.service << 'BTUNIT'
[Unit]
Description=Unblock Bluetooth RF-kill
DefaultDependencies=no
Before=bluetooth.service hciuart.service
After=sysinit.target

[Service]
Type=oneshot
ExecStart=/usr/sbin/rfkill unblock bluetooth

[Install]
WantedBy=multi-user.target
BTUNIT
systemctl enable rfkill-unblock-bluetooth.service 2>/dev/null || true

# Pick up dns=none and the new dispatcher with "nmcli general reload", never a
# full restart: a restart drops WiFi and kills SSH sessions mid-upgrade. The
# full effect (dns=none managing resolv.conf) lands at the reboot that always
# follows.
if systemctl is-active --quiet NetworkManager 2>/dev/null
then
  log_progress "Reloading NetworkManager configuration"
  nmcli general reload 2>/dev/null || true
fi

# Update /etc/fstab: root and boot read-only, volatile directories on tmpfs.
if ! grep -P -q "/boot\s+vfat\s+.+?(?=,ro)" /etc/fstab
then
  sed -i -r "s@(/boot\s+vfat\s+\S+)@\1,ro@" /etc/fstab
fi

if ! grep -P -q "/boot/firmware\s+vfat\s+.+?(?=,ro)" /etc/fstab
then
  sed -i -r "s@(/boot/firmware\s+vfat\s+\S+)@\1,ro@" /etc/fstab
fi

if ! grep -P -q "/\s+ext4\s+.+?(?=,ro)" /etc/fstab
then
  sed -i -r "s@(/\s+ext4\s+\S+)@\1,ro@" /etc/fstab
fi

if ! grep -w -q "/var/log" /etc/fstab
then
  echo "tmpfs /var/log tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

if ! grep -w -q "/var/tmp" /etc/fstab
then
  echo "tmpfs /var/tmp tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

if ! grep -w -q "/tmp" /etc/fstab
then
  echo "tmpfs /tmp    tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

if ! grep -w -q "/var/spool" /etc/fstab
then
  echo "tmpfs /var/spool tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

if ! grep -w -q "/var/lib/ntp" /etc/fstab
then
  if [ ! -d /var/lib/ntp ]
  then
    rm -rf /var/lib/ntp
    mkdir -p /var/lib/ntp
  fi
  echo "tmpfs /var/lib/ntp tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

# Networking directories on tmpfs so they're always writable at boot,
# regardless of whether /mutable (potentially on USB) has mounted yet.
if ! grep -w -q "/var/lib/NetworkManager" /etc/fstab
then
  mkdir -p /var/lib/NetworkManager
  echo "tmpfs /var/lib/NetworkManager tmpfs nodev,nosuid,mode=0700 0 0" >> /etc/fstab
fi
if ! grep -w -q "/var/lib/dhcp" /etc/fstab
then
  mkdir -p /var/lib/dhcp
  echo "tmpfs /var/lib/dhcp tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi
if ! grep -w -q "/var/lib/dhcpcd" /etc/fstab
then
  mkdir -p /var/lib/dhcpcd
  echo "tmpfs /var/lib/dhcpcd tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

# Put rfkill state on tmpfs so systemd-rfkill can't restore a stale soft-block
# captured while the root filesystem was being made read-only. Otherwise, if
# Bluetooth happened to be blocked at that moment, it stays blocked on every
# boot with "operation not possible due to RF-kill".
if ! grep -w -q "/var/lib/systemd/rfkill" /etc/fstab
then
  mkdir -p /var/lib/systemd/rfkill
  echo "tmpfs /var/lib/systemd/rfkill tmpfs nodev,nosuid 0 0" >> /etc/fstab
fi

# Suppress the 'mount' warning printed when /etc/fstab is newer than
# /run/systemd/systemd-units-load.
touch -t 197001010000 /etc/fstab

# autofs pulls in network-service dependencies by default because it can
# automount NFS. Here it only serves local snapshot loopback mounts
# (/tmp/snapshots), so dropping Wants=/After= speeds up startup.
if [ ! -e /etc/systemd/system/autofs.service ]
then
  grep -v '^Wants=\|^After=' /lib/systemd/system/autofs.service  > /etc/systemd/system/autofs.service
fi

log_progress "done"
