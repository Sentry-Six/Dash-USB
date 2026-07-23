#!/bin/bash -e

# ── SentryUSB Image Setup ──
# This runs inside pi-gen's chroot during image build.
# Goal: produce an image where the user flashes, boots, and gets a web UI.

touch "${ROOTFS_DIR}/boot/ssh"

# Remove firstrun.sh and the firstboot init hook. WiFi/hostname setup is
# handled by the SentryUSB iOS app via BLE, so Raspberry Pi Imager
# customization is not needed. Stripping the firstboot init= parameter
# prevents the Bookworm initramfs from auto-expanding the root partition
# to fill the entire disk — the setup script needs that free space for
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
install -m 666 files/sentryusb.conf.sample                "${ROOTFS_DIR}/boot/firmware/sentryusb.conf"
install -m 666 files/wpa_supplicant.conf.sample           "${ROOTFS_DIR}/boot/firmware"
install -m 666 files/run_once                             "${ROOTFS_DIR}/boot/firmware"
install -d "${ROOTFS_DIR}/root/bin"
install -d "${ROOTFS_DIR}/opt/sentryusb"

# Create /sentryusb symlink → /boot/firmware
ln -sf /boot/firmware "${ROOTFS_DIR}/sentryusb"

# ensure dwc2 module is loaded for USB gadget
echo "dtoverlay=dwc2" >> "${ROOTFS_DIR}/boot/firmware/config.txt"

# ── Pre-install SentryUSB binary variants + picker ──
#
# On aarch64 images we stage three per-CPU-tuned variants (a53/a72/a76).
# The runtime picker (installed below) symlinks the right one to
# sentryusb-current at every service start. On armv7 images there's
# a single variant, but the same picker handles both cases.
#
# armv6 (armel) is no longer supported — the original Pi Zero W and Pi 1
# don't have the headroom to run the daemon; image builds for those
# boards aren't produced anymore.
REPO="Sentry-Six/Sentry-USB-Rusty"
case "$(dpkg --print-architecture 2>/dev/null || echo arm64)" in
    arm64|aarch64) SUFFIXES="linux-arm64-a53 linux-arm64-a72 linux-arm64-a76" ;;
    armhf)         SUFFIXES="linux-armv7" ;;
    *)             SUFFIXES="linux-arm64-a72" ;;  # safe default
esac

for sfx in $SUFFIXES; do
    DEST="${ROOTFS_DIR}/opt/sentryusb/sentryusb-${sfx}"
    # Three input paths, preferred order — env override > injected file >
    # release download. The env override is only meaningful in CI, where
    # the build script can point at a freshly-cross-compiled binary by
    # setting SENTRYUSB_BINARY_LINUX_ARM64_A72 (etc.) — uppercase, dashes
    # to underscores.
    env_var="SENTRYUSB_BINARY_$(echo "$sfx" | tr 'a-z-' 'A-Z_')"
    env_val="${!env_var:-}"
    if [ -n "${env_val}" ] && [ -f "${env_val}" ]; then
        cp "${env_val}" "${DEST}"
    elif [ -f "files/sentryusb-${sfx}" ]; then
        cp "files/sentryusb-${sfx}" "${DEST}"
    elif [ -f "files/sentryusb-binary" ] && [ "${sfx}" = "$(echo $SUFFIXES | awk '{print $1}')" ]; then
        # Back-compat: build-image.sh's pre-multi-binary path drops a single
        # binary as files/sentryusb-binary. Use it for the first suffix; the
        # other variants will be missing (the picker's fallback chain handles
        # this — the daemon still runs, just without the per-CPU optimization).
        cp "files/sentryusb-binary" "${DEST}"
    else
        URL="https://github.com/${REPO}/releases/latest/download/sentryusb-${sfx}"
        curl -fsSL "${URL}" -o "${DEST}" || {
            echo "WARNING: Could not download sentryusb-${sfx} from releases. Picker will fall back."
            rm -f "${DEST}"
            continue
        }
    fi
    chmod +x "${DEST}"
done

# Install the picker script (selects the right variant at every boot).
install -m 755 "files/sentryusb-pick-binary" "${ROOTFS_DIR}/usr/local/bin/sentryusb-pick-binary"

# Write version file
RELEASE_TAG=$(curl -fsSL --max-time 10 "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
    | grep '"tag_name"' | head -1 \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' || true)
if [ -n "${RELEASE_TAG:-}" ]; then
    echo "$RELEASE_TAG" > "${ROOTFS_DIR}/opt/sentryusb/version"
    echo "Version: $RELEASE_TAG"
fi

# ── Pre-install Tesla BLE aux binaries (action CLI) ──
#
# sentryusb-ble-action: one-shot CLI invoked by run/awake_start to send
# keep-awake commands (wake / sentry-mode / charge-port). Without it,
# users with BLE keep-awake enabled hit "No such file or directory"
# errors on every nudge attempt.
#
# Same per-CPU variant scheme as the main binary above: stage every
# variant under /opt/sentryusb and let the runtime picker
# (sentryusb-pick-binary) symlink /root/bin/<name> to the right one at
# every boot. The old scheme — a single fixed binary copied straight
# into /root/bin — is what caused the wrong-variant SIGILL crash-loop
# on Pi 4 (issue #88): /root/bin was written once and never
# re-validated against the running CPU. (It was also broken in a
# second way: the release-download fallback interpolated
# ${BINARY_SUFFIX}, a variable nothing ever set, and the files/
# fallback looked for unsuffixed names while build-image.sh stages
# per-variant suffixed ones — so fresh images usually shipped with no
# aux binaries at all and leaned entirely on the OTA updater.)
#
# Input precedence per variant (mirrors the main-binary loop):
#   * files/<name>-<sfx> — per-variant build injected by build-image.sh
#   * SENTRYUSB_BLE_ACTION_BINARY env override — a single binary staged
#     under ALL variant slots; only fully correct for single-variant
#     builds (armv7), kept for CI and local-dev convenience
#   * GitHub release download as the last resort
for name in sentryusb-ble-action; do
    case "$name" in
        sentryusb-ble-action)      OVERRIDE="${SENTRYUSB_BLE_ACTION_BINARY:-}" ;;
    esac
    STAGED_ANY=""
    for sfx in $SUFFIXES; do
        DEST="${ROOTFS_DIR}/opt/sentryusb/${name}-${sfx}"
        if [ -f "files/${name}-${sfx}" ]; then
            cp "files/${name}-${sfx}" "${DEST}"
        elif [ -n "${OVERRIDE}" ] && [ -f "${OVERRIDE}" ]; then
            cp "${OVERRIDE}" "${DEST}"
        else
            URL="https://github.com/${REPO}/releases/latest/download/${name}-${sfx}"
            curl -fsSL "${URL}" -o "${DEST}" 2>/dev/null || {
                echo "WARNING: Could not download ${name}-${sfx} from releases. Picker will fall back."
                rm -f "${DEST}"
                continue
            }
        fi
        chmod +x "${DEST}"
        STAGED_ANY="yes"
    done
    # Pre-point the /root/bin symlink at the first staged variant (a53
    # baseline on aarch64 — runs on every supported board) so the path
    # exists even before the picker's first run. The picker re-points
    # it to the best variant at every boot.
    if [ -n "${STAGED_ANY}" ]; then
        for sfx in $SUFFIXES; do
            if [ -x "${ROOTFS_DIR}/opt/sentryusb/${name}-${sfx}" ]; then
                ln -sfn "/opt/sentryusb/${name}-${sfx}" "${ROOTFS_DIR}/root/bin/${name}"
                break
            fi
        done
    else
        echo "WARNING: no ${name} variant staged — the in-app updater installs it on first update; until then the dependent service/script stays inactive."
    fi
done

# ── Install BLE peripheral daemon ──
BLE_SCRIPT="${ROOTFS_DIR}/root/bin/sentryusb-ble.py"
if [ -f "files/sentryusb-ble.py" ]; then
    cp "files/sentryusb-ble.py" "${BLE_SCRIPT}"
elif [ -f "../../server/ble/sentryusb-ble.py" ]; then
    cp "../../server/ble/sentryusb-ble.py" "${BLE_SCRIPT}"
else
    curl -fsSL "https://raw.githubusercontent.com/${REPO}/main-dev/server/ble/sentryusb-ble.py" \
        -o "${BLE_SCRIPT}" 2>/dev/null || echo "WARNING: Could not fetch BLE daemon script"
fi
chmod +x "${BLE_SCRIPT}" 2>/dev/null || true

# ── Install D-Bus policy for BLE daemon (required on Pi 5 / Bookworm) ──
DBUS_CONF="${ROOTFS_DIR}/etc/dbus-1/system.d/com.sentryusb.ble.conf"
if [ -f "files/com.sentryusb.ble.conf" ]; then
    install -m 644 "files/com.sentryusb.ble.conf" "${DBUS_CONF}"
elif [ -f "../../server/ble/com.sentryusb.ble.conf" ]; then
    install -m 644 "../../server/ble/com.sentryusb.ble.conf" "${DBUS_CONF}"
else
    echo "WARNING: D-Bus policy file not found — BLE may fail on Pi 5"
fi

# ── (tesla-control / tesla-keygen are no longer installed) ──
# Keep Awake BLE, pairing, keygen and every Tesla command are native now
# (sentryusb-ble-action, installed via the variant binaries above). The
# external Go binaries are gone, so there is nothing to drop under
# /root/bin here.

# ── Install remountfs_rw helper (needed by BLE daemon to save PIN on read-only rootfs) ──
if [ -f "../../run/remountfs_rw" ]; then
    install -m 755 "../../run/remountfs_rw" "${ROOTFS_DIR}/root/bin/remountfs_rw"
else
    # Inline fallback so the image always has this script
    cat > "${ROOTFS_DIR}/root/bin/remountfs_rw" << 'RWEOF'
#!/bin/bash
mount / -o remount,rw
for _mp in /sentryusb /teslausb; do
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
# setup-sentryusb has run. setup-sentryusb keeps an idempotent copy of
# this block so upgrades to existing installs land it too.
if ! grep -q SENTRYUSB_TIP1 "${ROOTFS_DIR}/root/.bashrc" 2>/dev/null; then
    cat >> "${ROOTFS_DIR}/root/.bashrc" <<- 'EOC'
	if [ -n "$PS1" ]; then
		cat << SENTRYUSB_TIP1
		The root partition is mounted read-only.
		Run 'bin/remountfs_rw' to allow writing to it.

		SENTRYUSB_TIP1
	fi
	EOC
fi

BLE_SERVICE="${ROOTFS_DIR}/lib/systemd/system/sentryusb-ble.service"
if [ -f "files/sentryusb-ble.service" ]; then
    cp "files/sentryusb-ble.service" "${BLE_SERVICE}"
elif [ -f "../../server/ble/sentryusb-ble.service" ]; then
    cp "../../server/ble/sentryusb-ble.service" "${BLE_SERVICE}"
else
    curl -fsSL "https://raw.githubusercontent.com/${REPO}/main-dev/server/ble/sentryusb-ble.service" \
        -o "${BLE_SERVICE}" 2>/dev/null || echo "WARNING: Could not fetch BLE service file"
fi

# ── Install systemd service for the web UI ──
cat > "${ROOTFS_DIR}/lib/systemd/system/sentryusb.service" << 'SERVICEEOF'
[Unit]
Description=SentryUSB Web Server
After=mutable.mount backingfiles.mount
Wants=mutable.mount backingfiles.mount

[Service]
Type=simple
# Re-pick the best per-CPU binary on every start so a hardware swap
# (re-flashing the SD card into a different Pi) is handled automatically.
ExecStartPre=/usr/local/bin/sentryusb-pick-binary
ExecStart=/opt/sentryusb/sentryusb-current --port 80
Restart=always
RestartSec=5
# Per-crate log filter. Our crates emit at info; dependency chatter
# (hyper, h2, tokio, axum, etc.) stays at warn so journald isn't
# flooded with framework-level logs that nobody reads. Result: less
# write IO to the SD card, smaller journal footprint, less per-log
# CPU on Pi Zero 2 W.
Environment=RUST_LOG=sentryusb=info,sentryusb_api=info,sentryusb_drives=info,sentryusb_cloud_uploader=info,sentryusb_setup=info,sentryusb_gadget=info,sentryusb_notify=info,sentryusb_ws=info,sentryusb_cloud_crypto=info,tower_http=warn,warn
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
systemctl enable sentryusb.service
systemctl enable sentryusb-ble.service 2>/dev/null || true

# Install prerequisites needed by setup scripts
apt-get update -qq
apt-get install -y dos2unix parted fdisk sudo curl python3-dbus python3-gi

# Remove unwanted packages, disable unwanted services, and disable swap
# nginx conflicts with SentryUSB on port 80 — remove it to prevent fallback splash page
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
