# FAQ

## Which vehicles does this work with?

Any GM vehicle with the built-in **Surround Vision Recorder** dashcam (newer GM EVs and ICE vehicles) that records to a USB drive. The car records four cameras — front, left, right, rear — as 5-minute MP4 segments; 2027+ models add an interior camera, which Dash USB's viewer supports too.

Confirmed working on a 2026 model. GM's spec sheet says the drive must be USB 3.0, but the car accepts the Pi's USB 2.0 gadget fine.

## Does this void my GM warranty?

Consult your warranty terms or GM for warranty guidance. The Pi connects through
a regular USB-C port and writes only to its own storage.

## Does GM officially support this?

No. Dash USB is a third-party project, not affiliated with General Motors.

## Does it cost anything?

The software is **free for noncommercial use** under the PolyForm Noncommercial 1.0.0 license (source-available). You only pay for the hardware (Pi + SD card + cable).

## Why does the car only keep 2 hours of footage?

GM firmware deletes recordings older than a rolling ~2 hours from **any** USB
drive, regardless of size. Dash USB snapshots them every 15 minutes and archives
them to your server. Preserved footage appears under **Viewer → Recordings** and
on the archive server.

## When does the car actually record?

While the vehicle is on/driving. The Surround Vision Recorder is a driving dashcam — it doesn't record while the car is parked and locked. Expect roughly 5 GB of footage per hour of driving (four cameras × ~106 MB per 5-minute segment). A tiny few-MB clip at the end of a drive is normal — that's the partial segment written when you switch the car off.

## Why does the dashcam drive have to be 64 GB?

The car checks for a drive of **at least 64 GB with 32 GB available** and refuses to record otherwise. Dash USB handles both requirements automatically: it presents a 64 GB FAT32 virtual drive and always keeps 32 GiB free on it as it archives and cleans up.

## Can I use it without internet?

Yes. Dash USB only needs internet for:
- **First-time setup** (downloads the binary, installs system packages).
- **Update checks and installs**.
- Some **archive backends** (rclone to cloud storage).

All local archive methods (CIFS, NFS, rsync to a LAN server) work offline.

## How often does it archive? Can I trigger manually?

The Pi snapshots the car's recordings **every 15 minutes** (configurable), and archives the snapshots **whenever it connects to a known WiFi network**. For most users, that means every time you park in your driveway or garage.

To trigger manually, open the web UI and click the **Archive Sync** action at the top of the **Settings** page.

## Can I run it on hardware other than a Raspberry Pi?

Recommended: Raspberry Pi 4B, Pi 5. Should work: Pi Zero 2 W, Pi 3 Model A+.

The installer also includes board-specific support for the Radxa ROCK 4C+;
the [Getting Started](Getting-Started) guide covers Raspberry Pi OS.

Not supported: Pi Zero W and Pi 1 (armv6, the installer will refuse), and the **Pi 3 Model B and B+**. The 3B/3B+ send their single USB channel through an onboard hub chip that provides Ethernet and the USB-A ports, so the micro-USB port is power-only and cannot act as a USB gadget. The 3A+ has no such chip, so its micro-USB port works.

Other hardware is undocumented; ask on [Discord](https://discord.gg/9QZEzVwdnt)
for community experience.

## Is there a phone app?

Mobile push notifications use the Sentry Connect service. Pair from **Settings →
Notifications** in the web UI (see [Notifications](Notifications)). Sentry
Connect's Bluetooth device pairing does not currently recognize Dash USB.
