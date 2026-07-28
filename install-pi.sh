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

# Legacy first arg `norootshrink` skipped the root-partition shrink when an
# external USB/NVMe drive supplied the storage. The shrink now lives in the
# binary's setup wizard and is skipped automatically when DATA_DRIVE is set
# (Storage step, or /root/dashusb.conf). Consume the arg here so it isn't
# mistaken for a local binary path.
case "${1:-}" in
    norootshrink|no-root-shrink|NOROOTSHRINK|norrotshrink)
        info "Note: '$1' was a Go-era install arg; in the Rust port,"
        info "  pick your external drive on the wizard's Storage step"
        info "  (sets DATA_DRIVE) to skip root-partition shrinking."
        shift || true
        ;;
esac

# ── Step 1: /dashusb Symlink ─────────────────────────────────────

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

# ── Step 2: Install DashUSB Binary(es) + Picker ──────────────────
#
# aarch64 stages three per-CPU-tuned variants (a53/a72/a76) so each Pi runs
# code matched to its microarchitecture; armv7 has a single variant. The
# runtime picker (dashusb-pick-binary, installed below) symlinks the best one
# to dashusb-current at every service start, using /proc/cpuinfo detection.
#
# armv6 (Pi Zero W / Pi 1) is refused: too underpowered for the daemon, and
# dropped from CI, so no release artifact exists.

mkdir -p "$INSTALL_DIR"

# Detect userspace arch first, only to decide which release files to
# download. The picker repeats this detection at boot.
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

# Map the family to the release suffixes to download.
case "$ARCH_FAMILY" in
    aarch64) SUFFIXES="linux-arm64-a53 linux-arm64-a72 linux-arm64-a76" ;;
    armv7)   SUFFIXES="linux-armv7" ;;
    amd64)   SUFFIXES="linux-amd64" ;;
esac

if [ -n "${1:-}" ] && [ -f "${1:-}" ]; then
    # Local-binary mode (dev builds): stage the one binary under every CPU
    # suffix so the picker always finds something. Production installs take
    # the download path below.
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

# ── Picker script (selects the right binary at every service start) ──
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

# Run the picker now so the -current symlink and active-variant file exist
# before systemd starts the service.
"$PICKER_DST" || error_exit "dashusb-pick-binary failed on first run — check journalctl"

# Keep the old path working for third-party tooling and shell wrappers that
# reference /opt/dashusb/dashusb.
ln -sfn "$INSTALL_DIR/dashusb-current" "$INSTALL_DIR/$BINARY_NAME"

# Ensure binary is on PATH
if [ ! -L /usr/local/bin/dashusb ]; then
    ln -sf "$INSTALL_DIR/dashusb-current" /usr/local/bin/dashusb
fi

# ── Step 3: Systemd Service ─────────────────────────────────────────

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
# Re-pick the best per-CPU binary on every start so a hardware swap
# (re-flashing the SD card into a different Pi) is handled automatically.
ExecStartPre=/usr/local/bin/dashusb-pick-binary
ExecStart=/opt/dashusb/dashusb-current --port 80
Restart=always
RestartSec=5
Environment=RUST_LOG=info
# Cap glibc malloc arenas to 2. Default on multicore ARM is 8× nproc
# arenas, each holding a fragmented heap fork that the kernel never
# reclaims. Steady-state RSS on Pi-class hardware drops ~40-50% with
# this cap, with no measurable throughput impact for our workload.
Environment=MALLOC_ARENA_MAX=2
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable dashusb
ok "dashusb.service installed and enabled"

# ── Step 3b: BLE daemon (Python) ───────────────────────────────────

info "Installing DashUSB BLE daemon..."
BLE_REPO_URL="https://raw.githubusercontent.com/${REPO}/main/server/ble"
# Install at /root/bin/: it matches both the vendored unit's hardcoded
# ExecStart path and what pi-gen's 00-run.sh installs, so the daemon is
# reachable whether the user flashed the image or ran install-pi.sh. Do NOT
# install elsewhere and sed-patch the unit; that can fail silently on older
# sed or under SELinux, leaving the service pointing at a missing path.
BLE_INSTALL_PATH="/root/bin/dashusb-ble.py"
mkdir -p /root/bin

if curl -fsSL "$BLE_REPO_URL/dashusb-ble.py" -o "$BLE_INSTALL_PATH" 2>/dev/null; then
    chmod +x "$BLE_INSTALL_PATH"
    curl -fsSL "$BLE_REPO_URL/dashusb-ble.service" -o /etc/systemd/system/dashusb-ble.service 2>/dev/null || true
    curl -fsSL "$BLE_REPO_URL/com.dashusb.ble.conf" -o /etc/dbus-1/system.d/com.dashusb.ble.conf 2>/dev/null || true

    apt-get install -y python3-dbus python3-gi bluez >/dev/null 2>&1 || warn "BLE daemon apt deps install failed — the daemon may not start"
    # Pi OS extras the unit leans on (rfkill in ExecStartPre; pi-bluetooth
    # wires up the UART firmware). Best effort: absent on non-Pi distros,
    # usually preinstalled on Pi OS.
    apt-get install -y rfkill >/dev/null 2>&1 || true
    apt-get install -y pi-bluetooth >/dev/null 2>&1 || true
    systemctl daemon-reload
    systemctl enable dashusb-ble 2>/dev/null || true
    # Reload (SIGHUP), NOT restart. Restarting dbus on Pi OS kills logind,
    # which kills any active SSH session and can wedge the box hard enough to
    # need a power cycle. SIGHUP makes dbus reread /etc/dbus-1/system.d/,
    # which is all the new policy file needs, without dropping clients.
    systemctl reload dbus 2>/dev/null || true
    ok "BLE daemon installed at $BLE_INSTALL_PATH"
else
    warn "Could not fetch BLE daemon — iOS app pairing will be unavailable"
fi

# Step 3b2 (EATT disable) moved to setup/pi/apply-runtime-patches.sh
# so existing pre-v3.11.x installs heal automatically on OTA. Step 3d2
# installs that helper and runs it.

# ── Step 3c: archiveloop ↔ gadget shim scripts ─────────────────────
#
# archiveloop calls /root/bin/enable_gadget.sh and disable_gadget.sh directly.
# On a pre-existing install those are real configfs scripts, and leaving them
# in place gives two concurrent writers to the same
# /sys/kernel/config/usb_gadget/dashusb tree: half-configured gadgets that
# enumerate without exposing LUNs.
#
# Overwrite them with thin curl shims so archiveloop drives the API instead.
# The shims are idempotent; enabling an already-enabled gadget is a no-op.

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

# archiveloop sources envsetup.sh at runtime to read /root/dashusb.conf and
# export CAM_MOUNT, ARCHIVE_*, and friends. The pi-gen image ships it; without
# this fetch, install-pi.sh users get dashusb-archive.service failing with
# "envsetup.sh: No such file or directory" until systemd gives up respawning.
if curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/setup/pi/envsetup.sh" \
       -o /root/bin/envsetup.sh 2>/dev/null; then
    chmod +x /root/bin/envsetup.sh
    ok "envsetup.sh installed (archiveloop runtime config)"
else
    warn "envsetup.sh fetch failed — dashusb-archive.service may crash on boot"
fi

# ── Step 3d: remountfs_rw helper + /root/.bashrc reminder ──────────
# The pi-gen image build creates `remountfs_rw`; non-pi-gen installs
# (DietPi/Armbian) never get it. The BLE daemon calls it to remount root RW
# before saving the pairing PIN and otherwise fails with "Failed to save PIN:
# No such file or directory: '/root/bin/remountfs_rw'", blocking BLE pairing
# from SC. Install a stub that works whether root is read-only (remounts) or
# already writable (no-op, exit 0).
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

# ── Step 3d2: install the runtime-patches script (called by OTA updater) ──
# Runs for everyone; each patch inside self-detects its own precondition
# (board, file presence). It lives at a stable path that update.rs invokes
# after every binary swap, so install-time fixes (BLE non-fatal-adv on
# BCM4345C0 / 4C+, EATT disable on all boards) heal automatically on update
# instead of silently rotting.
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
# Run it now so a first install gets the patches without waiting for an OTA
# update. Per-patch detection gates make this a no-op on other boards.
if [ -x "$PATCHES_DST" ]; then
    "$PATCHES_DST" || warn "runtime-patches first-run reported issues — see output above"
fi

# ── Step 3f: Rock Pi 4C+ (RK3399 / dwc3) hardware setup ────────────────────
# Detection-gated: a no-op on Raspberry Pi and every other non-4C+ board. On a
# Rock Pi 4C+ a generic install leaves three things broken, all fixed here so
# SC works with WiFi and BLE out of the box:
#   1. rfkill: the BLE daemon's unit calls /usr/sbin/rfkill, which DietPi's
#      minimal base omits, so dashusb-ble.service fails 203/EXEC.
#   2. dwc3 overlay puts the OTG port in PERIPHERAL/high-speed mode. Without
#      it /sys/class/udc is empty, so there is no USB mass-storage gadget and
#      the car never sees the dashcam drive.
#   3. BT+WiFi firmware (AP6256/BCM4345C0 combo) plus a legacy raw-HCI LE
#      advertiser: the chip rejects BlueZ extended advertising, so SC can't
#      discover it.
# Best effort: each sub-step warns on failure rather than aborting the install.
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
    if [ -e "$BRCM/nvram_ap6256.txt" ]; then
        ln -sf nvram_ap6256.txt "$BRCM/brcmfmac43455-sdio.radxa,rock-4c-plus.txt"
        [ -e "$BRCM/brcmfmac43455-sdio.bin" ] && \
            ln -sf brcmfmac43455-sdio.bin "$BRCM/brcmfmac43455-sdio.radxa,rock-4c-plus.bin"
        [ -e "$BRCM/brcmfmac43455-sdio.clm_blob" ] && \
            ln -sf brcmfmac43455-sdio.clm_blob "$BRCM/brcmfmac43455-sdio.radxa,rock-4c-plus.clm_blob"
        ok "WiFi nvram → nvram_ap6256.txt (AP6256 calibration) — WiFi now survives BT"
        NEEDS_REBOOT=1
    else
        warn "nvram_ap6256.txt not found — WiFi may be unstable with BT (generic calibration)."
    fi

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

# ── Step 3g: BLE legacy-advertising helper (chip-gated install) ────
#
# Broadcom controllers in the BCM4345/43430/43438 family reject BlueZ's modern
# RegisterAdvertisement (or default to a scannable-but-non-connectable
# advertising type), so SC's connect attempt fails ~10s later with "GATT 147
# bond=BOND_NONE". The helper service installed here programs legacy ADV_IND
# (connectable) directly over raw HCI at 100ms intervals, plus a udev rule that
# brings the BLE stack up the moment hci0 appears (UART BT attaches late on
# cold boot).
#
# Install only where the chip is known affected. Pi 4 / Pi 5 (BCM43455 /
# CYW43455) are deliberately EXCLUDED: their modern bluetoothd path works, and
# this helper would override their good ext-adv with legacy adv. A Pi 4/5 user
# who does hit "GATT 147 bond=BOND_NONE" can opt in with:
#     sudo touch /mutable/force-ble-adv-helper
# That sentinel forces install regardless of chip detection.
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
        # $1 = filename, $2 = destination. Tries the local repo, then the URL.
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
        # Retire any older single-purpose unit from earlier installs.
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

# ── Step 3z: clean up a previous Sentry USB (Tesla) install ─────────
# Installing Dash USB onto a repurposed Sentry USB board must not leave the old
# product's services fighting this one. Idempotent; silent on fresh systems.
for unit in sentryusb sentryusb-archive sentryusb-ble sentryusb-telemetry sentryusb-ble-adv; do
    systemctl disable --now "$unit" >/dev/null 2>&1 || true
    rm -f "/etc/systemd/system/${unit}.service" "/lib/systemd/system/${unit}.service"
done
rm -f /root/bin/sentryusb* /root/bin/awake_start /root/bin/awake_stop \
      /root/bin/post-archive-process.sh /root/bin/copy-music.sh \
      /usr/local/bin/sentryusb* /etc/dbus-1/system.d/com.sentryusb.ble.conf \
      /etc/udev/rules.d/99-sentryusb-ble-hci.rules \
      /usr/local/bin/dashusb-ap-resurrect /etc/systemd/system/dashusb-ap-resurrect.service
rm -rf /opt/sentryusb /root/.ble
systemctl daemon-reload >/dev/null 2>&1 || true

# ── Step 4: Sample Config ───────────────────────────────────────────

if [ ! -f /root/dashusb.conf ]; then
    info "Creating sample config..."
    # The sample MUST come from this repo (Dash-USB). Any other source
    # returns a stale key set, or falls through to the stub below, and the web
    # UI's raw config editor then shows a handful of keys instead of the full
    # documented set.
    SAMPLE_URL="https://raw.githubusercontent.com/${REPO}/main/pi-gen-sources/00-dashusb-tweaks/files/dashusb.conf.sample"
    if curl -fsSL --max-time 15 "$SAMPLE_URL" -o /root/dashusb.conf; then
        ok "Sample config downloaded to /root/dashusb.conf"
    else
        # Fallback minimal template if offline/download fails.
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

# ── Step 5: WiFi Marker ────────────────────────────────────────────

if [ ! -f /dashusb/WIFI_ENABLED ]; then
    touch /dashusb/WIFI_ENABLED
fi

# ── Step 5b: Hostname + mDNS (dashusb.local works immediately) ───

TARGET_HOSTNAME="dashusb"
CURRENT_HOSTNAME=$(hostname -s 2>/dev/null || echo "raspberrypi")

if [ "$CURRENT_HOSTNAME" != "$TARGET_HOSTNAME" ]; then
    info "Setting hostname to ${TARGET_HOSTNAME}..."
    hostnamectl set-hostname "$TARGET_HOSTNAME" 2>/dev/null \
        || echo "$TARGET_HOSTNAME" > /etc/hostname
    # Update /etc/hosts so sudo/local lookups don't warn
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

# ── Step 6: Start the Service ──────────────────────────────────────

info "Starting DashUSB..."
systemctl restart dashusb

# Wait for an IP to report back; the network may have just bounced.
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
