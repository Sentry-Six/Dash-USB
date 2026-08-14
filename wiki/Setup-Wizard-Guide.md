# Setup Wizard Guide

The Setup Wizard runs the first time you open `http://dashusb.local`. You can
re-run it from **Settings → System → Setup Wizard**. Existing settings are the
starting point, and destructive storage changes require confirmation.

## 1. Welcome

Confirms the device is reachable. From here you can also **restore a configuration backup** (a `.json` backup exported from a previous Dash USB install) or upload an existing `dashusb.conf` — either pre-fills every later step.

Click **Get Started**.

## 2. Privacy

Lists outbound data flows and offers an **Analytics opt-in** choice. Leaving it
unanswered remains opted out.

- **Opted out (default)** — no fingerprint is included in future update checks;
  identifier-free checks continue.
- **Opted in** — a one-way salted hash of the board serial is attached to
  update-check telemetry for unique-install counts.

Either choice takes effect immediately and persists if you back out of the wizard. You can change it any time at **Settings → System → Analytics opt-in**. Full per-flow disclosure is on the [Privacy](Privacy) page.

## 3. Network

- **WiFi** — configured during SD card imaging (Raspberry Pi Imager), not here. To change it later, re-flash with new settings or use `nmcli` over SSH.
- **Device Hostname** — defaults to `dashusb`. Leave it unless you have a reason to change. The Pi is reachable at `http://<hostname>.local`.

## 4. Storage

- **Dashcam Size** — the virtual USB drive the car records to. Keep the 64 GB
  default unless necessary: GM requires ≥64 GB total with 32 GB available, and
  remaining capacity is used for retained snapshots.
- **External Data Drive** (optional) — store recordings on a USB or NVMe drive
  instead of the SD card. **The selected drive will be wiped.**

The drive is always FAT32 — that's what GM requires, and Dash USB handles the formatting.

## 5. Archive

Choose where recordings are backed up. Without an archive, snapshots remain
local and rotate as space is needed.

| Option | What it is |
|--------|-----------|
| **CIFS / SMB** | Network share on a Windows PC, Mac, or NAS |
| **rsync** | SSH-based file sync — for Linux/Unix servers |
| **rclone** | Cloud storage (Google Drive, S3, Backblaze, Dropbox, etc.) |
| **NFS** | Network File System — common on Linux NAS devices |
| **None** | No archiving — snapshots stay on the SD card until space runs out |

For rsync, the wizard generates an SSH key for the Pi and shows you the public key to paste onto your server, then lets you test the connection. See [Archive Methods](Archive-Methods) for setup details for each backend.

## 6. Notifications

Pick one or more push notification providers. Dash USB will notify you when archiving starts, finishes, or fails, and when temperature thresholds trip.

Credential-based providers are enabled by filling their fields; Mobile App has
an explicit checkbox. The wizard rejects incomplete credentials. See
[Notifications](Notifications) for provider setup.

## 7. Security

Set a **Web Username** and **Web Password** for the web UI. Set both or neither — the wizard won't let you fill in just one.

Leave both empty to disable web auth entirely — only do this if your network is fully trusted.

## 8. Advanced

- **Time Zone** — pick yours from the searchable list, or leave it on `auto` (used for log timestamps and notification times).
- **Archive Delay (seconds)** — how long to wait after Wi-Fi connects before
  archiving starts (default: 20).
- **Snapshot Interval (seconds)** — how often the Pi snapshots the recordings the car has written. Default **900** (15 minutes) gives ~8 capture chances per segment inside GM's 2-hour rolling-delete window. Lower it for a smaller worst-case capture gap; don't raise it above roughly half the rolling window or footage can age out before it's captured.
- **Measurement System** — Metric or Imperial for temperature readouts and the temperature monitor thresholds.
- **Temperature Monitoring** — optional Warning and Caution thresholds, a fixed
  log interval, and a post-archive temperature notification toggle.
- **RTC Battery** (Pi 5 only) — enable the Pi 5's built-in real-time clock if you've fitted a battery on the J5 header; trickle charging is offered behind an explicit "my battery is rechargeable" acknowledgement.
- **System Tuning** — Increase Root Size (fresh installs only), CPU Governor.
- **Update Source** — GitHub repo for OTA updates, plus the tracking branch for non-binary support files (runtime patches, migration fallback). Binaries always come from tagged Releases. Leave at the defaults unless you run a fork.

## 9. Review

Final summary of every choice. Click **Apply & Run Setup** to write the configuration and run setup — the Pi reboots several times during this, which is normal, and the page reconnects automatically.

Before applying, the wizard:

- **Checks free space** — incompatible drive sizes produce an inline error and a
  link to snapshot management.
- **Detects destructive changes** — changes that recreate disk images require
  explicit confirmation or can be skipped.

When it finishes, the Pi comes back up at `http://dashusb.local` (or your custom hostname).
