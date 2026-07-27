#!/bin/bash -e

# ── DashUSB Image Setup ──
# This runs inside pi-gen's chroot during image build.
# Goal: produce an image where the user flashes, boots, and gets a web UI.

touch "${ROOTFS_DIR}/boot/ssh"

# Remove firstrun.sh and the firstboot init hook. WiFi/hostname setup is
# handled by the DashUSB iOS app via BLE, so Raspberry Pi Imager
# customization is not needed. Stripping the firstboot init= parameter
# prevents the Bookworm initramfs from auto-expanding the root partition
# to fill the entire disk. The setup script needs that free space for
# backingfiles and mutable partitions.
rm -f "${ROOTFS_DIR}/boot/firmware/firstrun.sh"
rm -f "${ROOTFS_DIR}/boot/firmware/userconf.txt"
rm -f "${ROOTFS_DIR}/boot/firmware/custom.toml"
if [ -f "${ROOTFS_DIR}/boot/firmware/cmdline.txt" ]; then
    sed -i \
        -e 's| systemd\.run=/boot/firmware/firstrun\.sh||g' \
        -e 's| systemd\.run=/boot/firstrun\.sh||g' \
        -e 's| systemd\.run_success_action=reboot||g' \
        -e 's| systemd\.unit=kernel-command-line\.target||g' \
        -e 's| init=/usr/lib/raspberrypi-sys-mods/firstboot||g' \
        "${ROOTFS_DIR}/boot/firmware/cmdline.txt"
fi

install -m 755 files/rc.local                             "${ROOTFS_DIR}/etc/"
install -m 666 files/dashusb.conf.sample                "${ROOTFS_DIR}/boot/firmware/dashusb.conf.sample"
install -m 666 files/wpa_supplicant.conf.sample           "${ROOTFS_DIR}/boot/firmware"
install -m 666 files/run_once                             "${ROOTFS_DIR}/boot/firmware"
install -d "${ROOTFS_DIR}/root/bin"
install -d "${ROOTFS_DIR}/opt/dashusb"

# Create /dashusb symlink → /boot/firmware
ln -sf /boot/firmware "${ROOTFS_DIR}/dashusb"

# ensure dwc2 module is loaded for USB gadget
echo "dtoverlay=dwc2" >> "${ROOTFS_DIR}/boot/firmware/config.txt"

# ── Pre-install DashUSB binary variants + picker ──
#
# On aarch64 images we stage three per-CPU-tuned variants (a53/a72/a76).
# The runtime picker (installed below) symlinks the right one to
# dashusb-current at every service start. On armv7 images there's
# a single variant, but the same picker handles both cases.
#
# armv6 (armel) is no longer supported. The original Pi Zero W and Pi 1
# don't have the headroom to run the daemon; image builds for those
# boards aren't produced anymore.
REPO="Sentry-Six/Dash-USB"
case "$(dpkg --print-architecture 2>/dev/null || echo arm64)" in
    arm64|aarch64) SUFFIXES="linux-arm64-a53 linux-arm64-a72 linux-arm64-a76" ;;
    armhf)         SUFFIXES="linux-armv7" ;;
    *)             SUFFIXES="linux-arm64-a72" ;;  # safe default
esac

for sfx in $SUFFIXES; do
    DEST="${ROOTFS_DIR}/opt/dashusb/dashusb-${sfx}"
    # Three input paths, preferred order: env override > injected file >
    # release download. The env override is only meaningful in CI, where
    # the build script can point at a freshly-cross-compiled binary by
    # setting DASHUSB_BINARY_LINUX_ARM64_A72 (etc.): uppercase, dashes
    # to underscores.
    env_var="DASHUSB_BINARY_$(echo "$sfx" | tr 'a-z-' 'A-Z_')"
    env_val="${!env_var:-}"
    if [ -n "${env_val}" ] && [ -f "${env_val}" ]; then
        cp "${env_val}" "${DEST}"
    elif [ -f "files/dashusb-${sfx}" ]; then
        cp "files/dashusb-${sfx}" "${DEST}"
    elif [ -f "files/dashusb-binary" ] && [ "${sfx}" = "$(echo $SUFFIXES | awk '{print $1}')" ]; then
        # Back-compat: build-image.sh's pre-multi-binary path drops a single
        # binary as files/dashusb-binary. Use it for the first suffix; the
        # other variants will be missing (the picker's fallback chain handles
        # this; the daemon still runs, just without the per-CPU optimization).
        cp "files/dashusb-binary" "${DEST}"
    else
        URL="https://github.com/${REPO}/releases/latest/download/dashusb-${sfx}"
        curl -fsSL "${URL}" -o "${DEST}" || {
            echo "WARNING: Could not download dashusb-${sfx} from releases. Picker will fall back."
            rm -f "${DEST}"
            continue
        }
    fi
    chmod +x "${DEST}"
done

# Install the picker script (selects the right variant at every boot).
install -m 755 "files/dashusb-pick-binary" "${ROOTFS_DIR}/usr/local/bin/dashusb-pick-binary"

# Write version file
RELEASE_TAG=$(curl -fsSL --max-time 10 "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
    | grep '"tag_name"' | head -1 \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' || true)
if [ -n "${RELEASE_TAG:-}" ]; then
    echo "$RELEASE_TAG" > "${ROOTFS_DIR}/opt/dashusb/version"
    echo "Version: $RELEASE_TAG"
fi

# ── Install remountfs_rw helper (needed by BLE daemon to save PIN on read-only rootfs) ──
if [ -f "../../run/remountfs_rw" ]; then
    install -m 755 "../../run/remountfs_rw" "${ROOTFS_DIR}/root/bin/remountfs_rw"
else
    # Inline fallback so the image always has this script
    cat > "${ROOTFS_DIR}/root/bin/remountfs_rw" << 'RWEOF'
#!/bin/bash
mount / -o remount,rw
for _mp in /dashusb /teslausb; do
  if findmnt "$_mp" > /dev/null 2>&1; then
    mount "$_mp" -o remount,rw
    break
  fi
done
RWEOF
    chmod +x "${ROOTFS_DIR}/root/bin/remountfs_rw"
fi

# ── /root/.bashrc reminder pointing at bin/remountfs_rw ──
# Baked into the image so the tip prints on every `sudo -i` even before
# setup-dashusb has run. setup-dashusb keeps an idempotent copy of
# this block so upgrades to existing installs land it too.
if ! grep -q DASHUSB_TIP1 "${ROOTFS_DIR}/root/.bashrc" 2>/dev/null; then
    cat >> "${ROOTFS_DIR}/root/.bashrc" <<- 'EOC'
	if [ -n "$PS1" ]; then
		cat << DASHUSB_TIP1
		The root partition is mounted read-only.
		Run 'bin/remountfs_rw' to allow writing to it.

		DASHUSB_TIP1
	fi
	EOC
fi

BLE_SERVICE="${ROOTFS_DIR}/lib/systemd/system/dashusb-ble.service"
if [ -f "files/dashusb-ble.service" ]; then
    cp "files/dashusb-ble.service" "${BLE_SERVICE}"
elif [ -f "../../server/ble/dashusb-ble.service" ]; then
    cp "../../server/ble/dashusb-ble.service" "${BLE_SERVICE}"
else
    curl -fsSL "https://raw.githubusercontent.com/${REPO}/main-dev/server/ble/dashusb-ble.service" \
        -o "${BLE_SERVICE}" 2>/dev/null || echo "WARNING: Could not fetch BLE service file"
fi

# The daemon the unit executes, and its dbus policy. Without these the
# enabled service just crash-loops and phone provisioning is dead on a
# fresh image (no Ethernet fallback on a Zero 2 W).
mkdir -p "${ROOTFS_DIR}/root/bin" "${ROOTFS_DIR}/etc/dbus-1/system.d"
for src_dir in files ../../server/ble; do
    [ -f "${src_dir}/dashusb-ble.py" ] && install -m 755 "${src_dir}/dashusb-ble.py" "${ROOTFS_DIR}/root/bin/dashusb-ble.py" && break
done
for src_dir in files ../../server/ble; do
    [ -f "${src_dir}/com.dashusb.ble.conf" ] && install -m 644 "${src_dir}/com.dashusb.ble.conf" "${ROOTFS_DIR}/etc/dbus-1/system.d/com.dashusb.ble.conf" && break
done

# envsetup.sh: archiveloop sources /root/bin/envsetup.sh unconditionally;
# the Rust setup installs it on wizard runs, but the image must carry it
# so the archive service can start before/without a re-run.
if [ -f "../../setup/pi/envsetup.sh" ]; then
    install -m 755 "../../setup/pi/envsetup.sh" "${ROOTFS_DIR}/root/bin/envsetup.sh"
fi

# ── Install systemd service for the web UI ──
cat > "${ROOTFS_DIR}/lib/systemd/system/dashusb.service" << 'SERVICEEOF'
[Unit]
Description=DashUSB Web Server
After=mutable.mount backingfiles.mount
Wants=mutable.mount backingfiles.mount

[Service]
Type=simple
# Re-pick the best per-CPU binary on every start so a hardware swap
# (re-flashing the SD card into a different Pi) is handled automatically.
ExecStartPre=/usr/local/bin/dashusb-pick-binary
ExecStart=/opt/dashusb/dashusb-current --port 80
Restart=always
RestartSec=5
# Per-crate log filter. Our crates emit at info; dependency chatter
# (hyper, h2, tokio, axum, etc.) stays at warn so journald isn't
# flooded with framework-level logs that nobody reads. Result: less
# write IO to the SD card, smaller journal footprint, less per-log
# CPU on Pi Zero 2 W.
Environment=RUST_LOG=dashusb=info,sentryusb_api=info,sentryusb_setup=info,sentryusb_gadget=info,sentryusb_notify=info,sentryusb_ws=info,sentryusb_vehicle_profile=info,tower_http=warn,warn
# Cap glibc malloc arenas to 2. Default on multicore ARM is 8× nproc
# arenas, each holding a fragmented heap fork that the kernel never
# reclaims. Steady-state RSS on Pi-class hardware drops ~40-50% with
# this cap, with no measurable throughput impact for our workload.
Environment=MALLOC_ARENA_MAX=2
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
SERVICEEOF

# ── Install prerequisite packages and clean up ──
on_chroot << EOF
# Enable the web server service
systemctl enable dashusb.service
systemctl enable dashusb-ble.service 2>/dev/null || true

# Install prerequisites needed by setup scripts
apt-get update -qq
apt-get install -y dos2unix parted fdisk sudo curl python3-dbus python3-gi

# Remove unwanted packages, disable unwanted services, and disable swap
# nginx conflicts with DashUSB on port 80; remove it to prevent a fallback splash page
apt-get remove -y --purge nginx nginx-common nginx-full 2>/dev/null || true
apt-get remove -y --purge triggerhappy userconf-pi dphys-swapfile firmware-libertas firmware-realtek firmware-atheros mkvtoolnix 2>/dev/null || true
apt-get -y autoremove
systemctl disable keyboard-setup || true
systemctl disable resize2fs_once || true
systemctl disable dpkg-db-backup || true
update-rc.d resize2fs_once remove || true
rm -f /etc/init.d/resize2fs_once
update-initramfs -u || true

# Clean apt cache to reduce image size
apt-get clean
rm -rf /var/lib/apt/lists/*
EOF
