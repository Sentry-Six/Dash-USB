#!/bin/bash -eu
#
# DashUSB installer. Downloads the binary and installs the systemd service.
# The binary handles ALL setup (partitioning, disk images, system config)
# through the web UI setup wizard.
#
# Usage:
#   sudo -i
#   curl -fsSL https://raw.githubusercontent.com/Sentry-Six/Dash-USB/main/install-pi.sh | bash
#
# Or with a local binary:
#   bash install-pi.sh /path/to/dashusb-binary

REPO="${REPO:-Sentry-Six/Dash-USB}"
INSTALL_DIR="/opt/dashusb"
BINARY_NAME="dashusb"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[0;33m'
NC='\033[0m'

info()  { echo -e "${BLUE}[INFO]${NC} $1"; }
ok()    { echo -e "${GREEN}[OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
error_exit() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

if [[ $EUID -ne 0 ]]; then
    error_exit "This script must be run as root. Try: sudo -i"
fi

# Consume the legacy `norootshrink` argument so it is not mistaken for a
# binary path; DATA_DRIVE now controls root-partition shrinking.
case "${1:-}" in
    norootshrink|no-root-shrink|NOROOTSHRINK|norrotshrink)
        info "Note: '$1' was a Go-era install arg; in the Rust port,"
        info "  pick your external drive on the wizard's Storage step"
        info "  (sets DATA_DRIVE) to skip root-partition shrinking."
        shift || true
        ;;
esac

info "Setting up /dashusb symlink..."
if [ ! -L /dashusb ]; then
    rm -rf /dashusb
    if [ -d /boot/firmware ] && findmnt --fstab /boot/firmware &> /dev/null; then
        ln -s /boot/firmware /dashusb
    else
        ln -s /boot /dashusb
    fi
fi
ok "/dashusb -> $(readlink /dashusb)"

# aarch64 installs a53/a72/a76 variants and selects one at service start;
# armv7 has one variant. armv6 has no CI artifact and is unsupported.

mkdir -p "$INSTALL_DIR"

# Use userspace architecture for release selection; the picker checks again.
if command -v dpkg >/dev/null 2>&1; then
    DPKG_ARCH=$(dpkg --print-architecture)
    case "$DPKG_ARCH" in
        arm64)  ARCH_FAMILY="aarch64" ;;
        armhf)  ARCH_FAMILY="armv7" ;;
        armel)  error_exit "Unsupported architecture: armel (armv6 / Pi Zero W / Pi 1). DashUSB requires Pi Zero 2 W or newer." ;;
        amd64)  ARCH_FAMILY="amd64" ;;
        *)      error_exit "Unsupported userspace architecture: $DPKG_ARCH" ;;
    esac
else
    case "$(uname -m)" in
        aarch64) ARCH_FAMILY="aarch64" ;;
        armv7l)  ARCH_FAMILY="armv7" ;;
        armv6l)  error_exit "Unsupported architecture: armv6l (Pi Zero W / Pi 1). DashUSB requires Pi Zero 2 W or newer." ;;
        x86_64)  ARCH_FAMILY="amd64" ;;
        *)       error_exit "Unsupported architecture: $(uname -m)" ;;
    esac
fi

case "$ARCH_FAMILY" in
    aarch64) SUFFIXES="linux-arm64-a53 linux-arm64-a72 linux-arm64-a76" ;;
    armv7)   SUFFIXES="linux-armv7" ;;
    amd64)   SUFFIXES="linux-amd64" ;;
esac

if [ -n "${1:-}" ] && [ -f "${1:-}" ]; then
    # Stage a development binary under every suffix so the picker finds it.
    info "Installing binary from local path: $1"
    for sfx in $SUFFIXES; do
        cp "$1" "$INSTALL_DIR/$BINARY_NAME-$sfx"
        chmod +x "$INSTALL_DIR/$BINARY_NAME-$sfx"
    done
    ok "Local binary staged under $(echo $SUFFIXES | tr ' ' '\n' | wc -l) variant(s)"
else
    info "Downloading DashUSB binary variants from GitHub..."

    for sfx in $SUFFIXES; do
        DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${BINARY_NAME}-${sfx}"
        TMP="/tmp/${BINARY_NAME}-${sfx}.new"
        success=false
        for attempt in $(seq 1 5); do
            if curl -fsSL "$DOWNLOAD_URL" -o "$TMP" 2>/dev/null; then
                chmod +x "$TMP"
                mv "$TMP" "$INSTALL_DIR/$BINARY_NAME-$sfx"
                ok "Downloaded $BINARY_NAME-$sfx"
                success=true
                break
            fi
            warn "Download of $sfx failed (attempt $attempt/5), retrying..."
            sleep 3
        done
        if [ "$success" != true ]; then
            error_exit "Failed to download $BINARY_NAME-$sfx after 5 attempts"
        fi
    done

    RELEASE_TAG=$(curl -fsSL --max-time 10 \
        "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | head -1 \
        | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' || true)
    if [ -n "${RELEASE_TAG:-}" ]; then
        echo "$RELEASE_TAG" > "$INSTALL_DIR/version"
        ok "Version: $RELEASE_TAG"
    fi
fi

# Select the matching CPU variant at every service start.
PICKER_URL="https://raw.githubusercontent.com/${REPO}/main/pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary"
PICKER_DST="/usr/local/bin/dashusb-pick-binary"
PICKER_LOCAL_FALLBACK="$(dirname "${1:-/dev/null}")/dashusb-pick-binary"
if [ -f "$PICKER_LOCAL_FALLBACK" ]; then
    install -m 755 "$PICKER_LOCAL_FALLBACK" "$PICKER_DST"
    ok "Picker installed from local path"
elif curl -fsSL --max-time 10 "$PICKER_URL" -o "$PICKER_DST" 2>/dev/null; then
    chmod +x "$PICKER_DST"
    ok "Picker downloaded to $PICKER_DST"
else
    error_exit "Failed to install dashusb-pick-binary — daemon won't start without it"
fi

# Create the active symlink before systemd starts the service.
"$PICKER_DST" || error_exit "dashusb-pick-binary failed on first run — check journalctl"

# Preserve the legacy binary path for external tooling.
ln -sfn "$INSTALL_DIR/dashusb-current" "$INSTALL_DIR/$BINARY_NAME"

if [ ! -L /usr/local/bin/dashusb ]; then
    ln -sf "$INSTALL_DIR/dashusb-current" /usr/local/bin/dashusb
fi

info "Installing systemd service..."

cat > /etc/systemd/system/dashusb.service << 'EOF'
[Unit]
Description=DashUSB Web Server
After=mutable.mount backingfiles.mount
Wants=mutable.mount backingfiles.mount
Conflicts=nginx.service

[Service]
Type=simple
ExecStartPre=-/bin/systemctl stop nginx
ExecStartPre=-/bin/systemctl disable nginx
# Re-select after an SD card moves to different hardware.
ExecStartPre=/usr/local/bin/dashusb-pick-binary
ExecStart=/opt/dashusb/dashusb-current --port 80
Restart=always
RestartSec=5
Environment=RUST_LOG=info
# Limit glibc arena fragmentation and steady-state RSS on Pi hardware.
Environment=MALLOC_ARENA_MAX=2
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable dashusb
ok "dashusb.service installed and enabled"

info "Installing DashUSB BLE daemon..."
BLE_REPO_URL="https://raw.githubusercontent.com/${REPO}/main/server/ble"
# Match the fixed ExecStart path used by the unit and pi-gen.
BLE_INSTALL_PATH="/root/bin/dashusb-ble.py"
mkdir -p /root/bin

if curl -fsSL "$BLE_REPO_URL/dashusb-ble.py" -o "$BLE_INSTALL_PATH" 2>/dev/null; then
    chmod +x "$BLE_INSTALL_PATH"
    curl -fsSL "$BLE_REPO_URL/dashusb-ble.service" -o /etc/systemd/system/dashusb-ble.service 2>/dev/null || true
    curl -fsSL "$BLE_REPO_URL/com.dashusb.ble.conf" -o /etc/dbus-1/system.d/com.dashusb.ble.conf 2>/dev/null || true

    apt-get install -y python3-dbus python3-gi bluez >/dev/null 2>&1 || warn "BLE daemon apt deps install failed — the daemon may not start"
    # Best-effort Pi OS dependencies; other distributions may not provide them.
    apt-get install -y rfkill >/dev/null 2>&1 || true
    apt-get install -y pi-bluetooth >/dev/null 2>&1 || true
    systemctl daemon-reload
    systemctl enable dashusb-ble 2>/dev/null || true
    # SIGHUP reloads policy without restarting dbus and killing logind/SSH.
    systemctl reload dbus 2>/dev/null || true
    ok "BLE daemon installed at $BLE_INSTALL_PATH"
else
    warn "Could not fetch BLE daemon — iOS app pairing will be unavailable"
fi

# Replace legacy configfs writers with idempotent API calls, preventing Rust
# and archiveloop from mutating the gadget concurrently.

info "Installing archiveloop gadget shims..."
mkdir -p /root/bin

cat > /root/bin/enable_gadget.sh <<'SHIM'
#!/bin/bash
# Rust DashUSB shim — archiveloop calls this; we forward to the Rust API.
# Loopback requests bypass the web auth middleware.
exec curl -fsS --max-time 30 -X POST http://127.0.0.1/api/system/gadget-enable
SHIM
chmod +x /root/bin/enable_gadget.sh

cat > /root/bin/disable_gadget.sh <<'SHIM'
#!/bin/bash
exec curl -fsS --max-time 30 -X POST http://127.0.0.1/api/system/gadget-disable
SHIM
chmod +x /root/bin/disable_gadget.sh

ok "Gadget shims installed at /root/bin/{enable,disable}_gadget.sh"

# install-pi users need the envsetup.sh that pi-gen images already include.
if curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/setup/pi/envsetup.sh" \
       -o /root/bin/envsetup.sh 2>/dev/null; then
    chmod +x /root/bin/envsetup.sh
    ok "envsetup.sh installed (archiveloop runtime config)"
else
    warn "envsetup.sh fetch failed — dashusb-archive.service may crash on boot"
fi

# Non-pi-gen installs need the remount helper used when BLE saves its PIN.
mkdir -p /root/bin
if [ ! -f /root/bin/remountfs_rw ]; then
    cat > /root/bin/remountfs_rw <<'REMOUNT_RW'
#!/bin/bash
# remount root RW (no-op if already RW). Used by dashusb-ble.py for PIN save.
mount -o remount,rw / 2>/dev/null
exit 0
REMOUNT_RW
    chmod +x /root/bin/remountfs_rw
    ok "Installed /root/bin/remountfs_rw stub (BLE daemon PIN save)"
fi
if ! grep -q DASHUSB_TIP1 /root/.bashrc 2>/dev/null; then
    cat >> /root/.bashrc <<- 'EOC'
	if [ -n "$PS1" ]; then
		cat << DASHUSB_TIP1
		The root partition is mounted read-only.
		Run 'bin/remountfs_rw' to allow writing to it.

		DASHUSB_TIP1
	fi
	EOC
    ok "Added remountfs_rw reminder to /root/.bashrc"
fi

# OTA invokes this detection-gated helper after each binary swap.
PATCHES_URL="https://raw.githubusercontent.com/${REPO}/main/setup/pi/apply-runtime-patches.sh"
PATCHES_DST="/usr/local/bin/dashusb-apply-runtime-patches"
PATCHES_LOCAL="$(dirname "${1:-/dev/null}")/setup/pi/apply-runtime-patches.sh"
if [ -f "$PATCHES_LOCAL" ]; then
    install -m 755 "$PATCHES_LOCAL" "$PATCHES_DST"
    ok "Runtime-patches script installed from local path"
elif curl -fsSL --max-time 10 "$PATCHES_URL" -o "$PATCHES_DST" 2>/dev/null; then
    chmod +x "$PATCHES_DST"
    ok "Runtime-patches script downloaded to $PATCHES_DST"
else
    warn "Could not fetch runtime-patches script — OTA updates won't re-apply BLE patches"
fi
# Apply patches now, using the same source as custom-REPO installs.
if [ -x "$PATCHES_DST" ]; then
    DASHUSB_REPO_SLUG="$REPO" DASHUSB_REF=main "$PATCHES_DST" \
        || warn "runtime-patches first-run reported issues — see output above"
fi

# Rock Pi 4C+ needs four detection-gated compatibility fixes:
#   1. rfkill: the BLE daemon's unit calls /usr/sbin/rfkill, which DietPi's
#      minimal base omits, so dashusb-ble.service fails 203/EXEC.
#   2. dwc3 overlay puts the OTG port in PERIPHERAL/high-speed mode. Without
#      it /sys/class/udc is empty, so there is no USB mass-storage gadget and
#      the car never sees the dashcam drive.
#   3. BT+WiFi firmware (AP6256/BCM4345C0 combo) plus a legacy raw-HCI LE
#      advertiser: the chip rejects BlueZ extended advertising, so SC can't
#      discover it.
# Each fix is best-effort.
is_rock_4cplus() {
    grep -qai 'rock-4c-plus\|rockpi4c-plus\|ROCK 4C+' \
        /proc/device-tree/model /proc/device-tree/compatible 2>/dev/null
}
has_dietpi_overlays() {
    [ -f /boot/dietpiEnv.txt ] && grep -q '^overlay_path=' /boot/dietpiEnv.txt
}

NEEDS_REBOOT=0
if is_rock_4cplus; then
    info "Rock Pi 4C+ detected — applying USB-gadget + BLE hardware setup..."
    # Best effort: don't let a minor apt/systemd hiccup abort the install.
    set +e

    # 1. rfkill (the BLE daemon calls it) and device-tree-compiler (sub-step 2
    #    compiles a dwc3 overlay with `dtc`). DietPi minimal ships neither.
    if apt-get install -y rfkill device-tree-compiler >/dev/null 2>&1; then
        ok "rfkill + device-tree-compiler installed"
        systemctl reset-failed dashusb-ble.service 2>/dev/null || true
        systemctl restart dashusb-ble.service 2>/dev/null || true
    else
        warn "rfkill/dtc install failed — BLE daemon and dwc3 overlay may not work"
    fi

    # 2. High-speed dwc3 peripheral overlay, compiled on-device so no
    #    prebuilt .dtbo has to ship.
    if has_dietpi_overlays; then
        apt-get install -y device-tree-compiler >/dev/null 2>&1 || true
        mkdir -p /boot/overlay-user
        cat > /tmp/dashusb-dwc3-hs.dts <<'DTS'
/dts-v1/;
/plugin/;
/ {
    metadata {
        title = "DashUSB: OTG peripheral high-speed (Rock 4C+)";
        compatible = "rockchip,rk3399";
        category = "misc";
        exclusive = "usbdrd_dwc3_0-dr_mode";
        description = "dwc3 OTG → peripheral mode, high-speed, for the USB gadget.";
    };
    fragment@0 {
        target = <&usbdrd_dwc3_0>;
        __overlay__ {
            status = "okay";
            dr_mode = "peripheral";
            maximum-speed = "high-speed";
        };
    };
};
DTS
        if dtc -@ -I dts -O dtb -o /boot/overlay-user/dashusb-dwc3-hs.dtbo \
               /tmp/dashusb-dwc3-hs.dts 2>/dev/null; then
            ok "Compiled high-speed dwc3 overlay → /boot/overlay-user/dashusb-dwc3-hs.dtbo"
            cur=$(grep '^user_overlays=' /boot/dietpiEnv.txt | cut -d= -f2-)
            case " $cur " in
                *" dashusb-dwc3-hs "*)
                    ok "Overlay already registered in user_overlays" ;;
                *)
                    new=$(echo "$cur dashusb-dwc3-hs" | xargs)
                    cp /boot/dietpiEnv.txt /boot/dietpiEnv.txt.dashusb.bak
                    sed -i "s/^user_overlays=.*/user_overlays=$new/" /boot/dietpiEnv.txt
                    ok "Registered overlay (user_overlays=$new)"
                    NEEDS_REBOOT=1 ;;
            esac
        else
            warn "dwc3 overlay compile failed — USB gadget will NOT appear until applied manually"
        fi
    else
        warn "Rock 4C+ but no DietPi/Armbian overlay mechanism found — apply a dwc3"
        warn "peripheral+high-speed overlay for your image manually, or no USB gadget."
    fi

    # 3. Bluetooth + WiFi firmware coexistence on the AP6256 (BCM4345C0
    #    WiFi+BT combo). The BT .hcd MUST be the GENERIC patch, never
    #    BCM4345C0.raspberrypi,*.hcd: the Pi profile kills the WiFi SDIO half
    #    (brcmf rxctl timeout / wlan0 I/O error).
    BRCM=/lib/firmware/brcm
    HCD=""
    for c in BCM4345C0_003.001.025.0162.0000_Generic_UART_37_4MHz_wlbga_ref_iLNA_iTR_eLG.hcd \
             BCM4345C0.raspberrypi,4-compute-module.hcd; do
        [ -e "$BRCM/$c" ] && { HCD="$c"; break; }
    done
    [ -z "$HCD" ] && HCD=$(cd "$BRCM" 2>/dev/null && ls BCM4345C0*.hcd 2>/dev/null | grep -vE 'radxa,rock-4c-plus|raspberrypi' | head -1)
    if [ -n "$HCD" ] && [ -e "$BRCM/$HCD" ]; then
        ln -sf "$HCD" "$BRCM/BCM4345C0.radxa,rock-4c-plus.hcd"
        ln -sf "$HCD" "$BRCM/BCM4345C0.hcd"
        ok "BT firmware → $HCD (generic AP6256 patch, NOT the Pi profile) — reboot to load"
        NEEDS_REBOOT=1
    else
        warn "BCM4345C0 .hcd not found — 'apt install --reinstall armbian-firmware', then"
        warn "symlink BCM4345C0.radxa,rock-4c-plus.hcd → the generic BCM4345C0 .hcd."
    fi
    # No WiFi NVRAM relink: nvram_ap6256.txt collapses 4C+ TX to ~6 Mbit/s
    # (sole TX-power source, no txcap_blob). Driver falls back to the generic
    # brcmfmac43455-sdio.txt. BT coexistence is the .hcd patch above, not this.

    # 4. Prefer OpenSSH over Dropbear: Dropbear ships no SFTP subsystem, so
    #    scp/sftp to the board fail.
    if command -v dropbear >/dev/null 2>&1 && [ -x /boot/dietpi/func/dietpi-set_software ]; then
        if /boot/dietpi/func/dietpi-set_software ssh-server openssh >/dev/null 2>&1; then
            ok "Switched SSH server to OpenSSH (scp/sftp support)"
        else
            warn "OpenSSH switch failed — Dropbear left in place (scp/sftp unavailable)"
        fi
    fi

    set -e  # end best-effort section
fi

# BCM4345/43430/43438 can reject BlueZ RegisterAdvertisement; the helper emits
# connectable legacy ADV_IND and starts when the late UART adapter appears.
# Pi 4/5 are excluded because the helper would override working extended
# advertising. Affected users can force installation with:
#     sudo touch /mutable/force-ble-adv-helper
is_known_broken_ble_chip() {
    [ -f /mutable/force-ble-adv-helper ] && return 0   # operator override
    local chips="BCM4345C0\|BCM43430B0\|BCM43438"
    dmesg 2>/dev/null | grep -qE "hci0: ($chips)" && return 0
    grep -qai 'rock-4c-plus\|rockpi4c-plus\|ROCK 4C+\|Raspberry Pi Zero 2 W\|Raspberry Pi 3 Model B\|Raspberry Pi Zero W' \
        /proc/device-tree/model 2>/dev/null && return 0
    return 1
}
if is_known_broken_ble_chip; then
    info "Known-affected BLE chip detected — installing raw-HCI advertising helper..."
    BLE_ADV_BASE_URL="https://raw.githubusercontent.com/${REPO}/main/setup/pi"
    LOCAL_PI_DIR="$(dirname "${1:-/dev/null}")/setup/pi"
    fetch_file() {
        # $1: filename; $2: destination. Prefer the local repository.
        if [ -f "$LOCAL_PI_DIR/$1" ]; then
            install -m 644 "$LOCAL_PI_DIR/$1" "$2"
        elif curl -fsSL --max-time 15 "$BLE_ADV_BASE_URL/$1" -o "$2" 2>/dev/null; then
            :
        else
            warn "Failed to fetch $1 — BLE LE advertising may not work"
            return 1
        fi
        return 0
    }
    if fetch_file dashusb-ble-adv.sh /usr/local/bin/dashusb-ble-adv.sh; then
        chmod +x /usr/local/bin/dashusb-ble-adv.sh
        fetch_file dashusb-ble-adv.service /etc/systemd/system/dashusb-ble-adv.service
        fetch_file 99-dashusb-ble-hci.rules /etc/udev/rules.d/99-dashusb-ble-hci.rules
        mkdir -p /etc/systemd/system/dashusb-ble.service.d
        fetch_file dashusb-ble-wants-bluetooth.conf /etc/systemd/system/dashusb-ble.service.d/wants-bluetooth.conf
        # Retire the superseded single-purpose unit.
        systemctl disable --now dashusb-ble-le.service 2>/dev/null || true
        rm -f /etc/systemd/system/dashusb-ble-le.service 2>/dev/null
        rm -rf /etc/systemd/system/dashusb-ble-le.service.d 2>/dev/null
        systemctl enable bluetooth.service >/dev/null 2>&1 || true
        systemctl daemon-reload 2>/dev/null || true
        udevadm control --reload-rules 2>/dev/null || true
        systemctl enable dashusb-ble-adv.service >/dev/null 2>&1 || true
        ok "BLE legacy-advertising helper installed (script + service + hci0 udev rule)"
    fi
fi

if [ ! -f /root/dashusb.conf ]; then
    info "Creating sample config..."
    # Fetch the matching key set from this repository.
    SAMPLE_URL="https://raw.githubusercontent.com/${REPO}/main/pi-gen-sources/00-dashusb-tweaks/files/dashusb.conf.sample"
    if curl -fsSL --max-time 15 "$SAMPLE_URL" -o /root/dashusb.conf; then
        ok "Sample config downloaded to /root/dashusb.conf"
    else
        # Minimal offline fallback.
        cat > /root/dashusb.conf << 'CONFEOF'
# DashUSB Configuration
# Edit these values and run setup from the web UI.
#
# Required — GM needs a 64 GB or larger drive with 32 GB available:
export CAM_SIZE=64G

# Archive system: none, cifs, nfs, rsync, rclone
#export ARCHIVE_SYSTEM=none

# Optional: WiFi access point (min 8 char password)
#export AP_SSID=DashUSB
#export AP_PASS=

# Optional: Hostname (default: dashusb)
#export DASHUSB_HOSTNAME=dashusb

# Optional: External USB drive instead of SD card
#export DATA_DRIVE=
CONFEOF
        ok "Sample config created at /root/dashusb.conf (offline fallback)"
    fi
fi

if [ ! -f /dashusb/WIFI_ENABLED ]; then
    touch /dashusb/WIFI_ENABLED
fi

TARGET_HOSTNAME="dashusb"
CURRENT_HOSTNAME=$(hostname -s 2>/dev/null || echo "raspberrypi")

if [ "$CURRENT_HOSTNAME" != "$TARGET_HOSTNAME" ]; then
    info "Setting hostname to ${TARGET_HOSTNAME}..."
    hostnamectl set-hostname "$TARGET_HOSTNAME" 2>/dev/null \
        || echo "$TARGET_HOSTNAME" > /etc/hostname
    if grep -qE "^127\.0\.1\.1\s" /etc/hosts; then
        sed -i "s/^127\.0\.1\.1\s.*/127.0.1.1\t${TARGET_HOSTNAME}/" /etc/hosts
    else
        echo -e "127.0.1.1\t${TARGET_HOSTNAME}" >> /etc/hosts
    fi
    hostname "$TARGET_HOSTNAME" 2>/dev/null || true
    ok "Hostname set to ${TARGET_HOSTNAME}"
fi

info "Ensuring avahi-daemon is installed for mDNS (${TARGET_HOSTNAME}.local)..."
if ! command -v avahi-daemon >/dev/null 2>&1; then
    apt-get install -y avahi-daemon >/dev/null 2>&1 \
        || warn "avahi-daemon install failed — ${TARGET_HOSTNAME}.local may not resolve"
fi
# Advertise IPv4 only: a AAAA answer for .local sends Windows/Chrome to the
# board's rotating SLAAC address (slow, stale) and triggers Chrome Private
# Network Access "CORS" blocks on the plain-http UI. Device IPv6 is untouched.
AVAHI_V4_URL="https://raw.githubusercontent.com/${REPO}/main/setup/pi/avahi-ipv4-only.sh"
if curl -fsSL --max-time 15 "$AVAHI_V4_URL" -o /tmp/avahi-ipv4-only.sh 2>/dev/null; then
    bash /tmp/avahi-ipv4-only.sh >/dev/null 2>&1 || warn "could not apply IPv4-only mDNS config"
    rm -f /tmp/avahi-ipv4-only.sh
else
    warn "could not fetch avahi-ipv4-only.sh — ${TARGET_HOSTNAME}.local may advertise IPv6"
fi
systemctl enable avahi-daemon >/dev/null 2>&1 || true
systemctl restart avahi-daemon >/dev/null 2>&1 || true
ok "mDNS active: http://${TARGET_HOSTNAME}.local"

info "Starting DashUSB..."
systemctl restart dashusb

# Wait for network recovery before reporting an address.
IP=""
for _ in $(seq 1 30); do
    IP=$(hostname -I 2>/dev/null | awk '{print $1}')
    [ -n "$IP" ] && break
    sleep 1
done
HOSTNAME="$TARGET_HOSTNAME"

echo ""
echo -e "${GREEN}╔════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║        DashUSB Installation Complete         ║${NC}"
echo -e "${GREEN}╚════════════════════════════════════════════════╝${NC}"
echo ""
if [ -n "$IP" ]; then
    echo -e "  Web UI:  ${BLUE}http://${IP}${NC}"
else
    echo -e "  Web UI:  ${YELLOW}(no IP detected — check 'ip a' once network is up)${NC}"
fi
echo -e "  mDNS:    ${BLUE}http://${HOSTNAME}.local${NC}"
echo ""
echo -e "  Open the web UI to complete setup via the wizard."
echo -e "  All setup (partitions, drives, etc.) is handled by the binary."
echo ""
echo -e "  Config:  /root/dashusb.conf"
echo -e "  Binary:  ${INSTALL_DIR}/dashusb-current → $(readlink "${INSTALL_DIR}/dashusb-current" 2>/dev/null || echo "<picker has not run yet>")"
echo -e "  Logs:    journalctl -u dashusb -f"
echo ""

if [ "${NEEDS_REBOOT:-0}" = "1" ]; then
    warn "Rock 4C+: a REBOOT is required to activate the USB gadget (dwc3 → peripheral)"
    warn "          and load the BT/WiFi firmware."
    echo -e "  Run:  ${BLUE}reboot${NC}  — afterward /sys/class/udc/ shows fe800000.usb, so"
    echo -e "        the car sees the drive and the BLE daemon can advertise."
    echo ""
fi
