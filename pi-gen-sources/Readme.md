## Building a Dash USB Image

> **Prebuilt images aren't offered right now.** Install Dash USB with the one-liner in the [README](../README.md#install). These image scripts are kept for a future image release and aren't maintained or tested in the meantime.

The Dash USB image is a custom Raspberry Pi OS image with everything pre-installed. Users just flash it, configure WiFi in Pi Imager, boot, and open the web UI — no SSH required.

### Quick Method (recommended)

From the repo root, run:

```bash
./build-image.sh
```

This will:
1. Build the CPU-tuned Dash USB binaries with `cross` (or download them from the latest release)
2. Clone pi-gen and inject the binary
3. Build the image using Docker
4. Output a compressed `.img.gz` in `deploy/`

Takes 15–30 minutes. Requires **Docker**.

You can also pass a pre-built binary (it is staged in every CPU variant slot), or add `--32bit` for an armhf image:
```bash
./build-image.sh /path/to/dashusb
```

### Manual Method

1. Clone pi-gen: `git clone https://github.com/RPi-Distro/pi-gen`
2. From the pi-gen folder, run: `bash /path/to/Dash-USB/pi-gen-sources/prepare.sh`
3. Optionally inject local binaries by copying them to `stage_dashusb/00-dashusb-tweaks/files/dashusb-linux-arm64-a53` (and `-a72`, `-a76`), and copy `server/ble/*` into the same folder; binaries you don't supply are downloaded from the latest release
4. Run `./build-docker.sh` (Docker recommended) or `./build.sh`
5. Image will be in `deploy/`. Flash with Raspberry Pi Imager.

### GitHub Actions (CI)

CI builds and publishes the `dashusb-*` binaries only; there is no image workflow. Build images locally with `./build-image.sh`.

### What's in the image

- Raspberry Pi OS Trixie Lite (64-bit arm64, or armhf with `--32bit`)
- DashUSB binaries pre-installed in `/opt/dashusb/` as per-CPU variants (`dashusb-linux-arm64-a53`/`-a72`/`-a76`); `dashusb-pick-binary` selects one at service start
- systemd service enabled (web UI on port 80)
- rc.local boot-loop for setup (creates partitions, configures archiving)
- SSH enabled, `dwc2` overlay for USB gadget
- BLE provisioning daemon (`dashusb-ble.service`)
- Prerequisite packages: dos2unix, parted, fdisk, curl, xfsprogs, autofs, rsync, bluez
- Unnecessary packages removed, swap disabled

### User experience after flashing

1. User flashes image with Pi Imager, configures WiFi + password in settings
2. Boots Pi → WiFi connects → Dash USB web server starts on port 80
3. User opens `http://dashusb.local` → sees the dashboard
4. Completes Setup Wizard → Pi reboots several times (10–20 min) → done
5. Plugs into the vehicle
