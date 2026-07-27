# Setup Wizard Guide

The Setup Wizard runs the first time you open `http://dashusb.local`. It walks you through 9 steps. You can re-run it anytime from **Settings → System → Setup Wizard** — your existing configuration is the starting point, and re-running it won't touch your footage unless you change drive sizes (the wizard warns you loudly before anything destructive).

## 1. Welcome

Confirms the device is reachable. From here you can also **restore a configuration backup** (a `.json` backup exported from a previous Dash USB install) or upload an existing `dashusb.conf` — either pre-fills every later step.

Click **Get Started**.

## 2. Privacy

Lists every outbound data flow Dash USB will make and asks you to confirm an **Analytics opt-in** choice before the wizard moves on. Both buttons carry equal visual weight — pick whichever you actually want.

- **Opted out (default)** — no device fingerprint ever leaves the Pi. Update checks still happen but carry no identifier.
- **Opted in** — a one-way salted hash of your board's serial number is attached to update-check telemetry, so we can count unique installs without double-counting reinstalls.

Either choice takes effect immediately and persists if you back out of the wizard. You can change it any time at **Settings → System → Analytics opt-in**. Full per-flow disclosure is on the [Privacy](Privacy) page.

## 3. Network

- **WiFi** — configured during SD card imaging (Raspberry Pi Imager), not here. To change it later, re-flash with new settings or use `nmcli` over SSH.
- **Device Hostname** — defaults to `dashusb`. Leave it unless you have a reason to change. The Pi is reachable at `http://<hostname>.local`.
- **WiFi Access Point** (optional) — broadcast a WiFi network from the Pi itself (SSID, password of 8+ characters, optional IP).

## 4. Storage

- **Dashcam Size** — the virtual USB drive the car records to. **Leave it at the 64 GB default.** GM refuses any drive under 64 GB (it checks for ≥64 GB total with 32 GB available), so the wizard won't accept less — and bigger isn't better: everything above the dashcam drive is used for **snapshots**, which are your retained footage history. The car itself only ever keeps ~2 hours on the drive, so give the snapshots the rest of the card.
- **External Data Drive** (optional) — store recordings on a USB or NVMe drive instead of the SD card. Best for heavy users. **The selected drive will be wiped.**

The drive is always FAT32 — that's what GM requires, and Dash USB handles the formatting.

## 5. Archive

Pick where your recordings get backed up. Because the car deletes everything older than ~2 hours, the archive is the only durable copy of your footage — "None" is rarely what you want.

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

A provider counts as enabled once you fill in its fields — there are no separate checkboxes, and the wizard flags partial fills (e.g. a Telegram chat ID without a bot token). See [Notifications](Notifications) for the full list of providers and how to get API keys for each.

## 7. Security

Set a **Web Username** and **Web Password** for the web UI. Set both or neither — the wizard won't let you fill in just one.

Leave both empty to disable web auth entirely — only do this if your network is fully trusted.

## 8. Advanced

- **Time Zone** — pick yours from the searchable list, or leave it on `auto` (used for log timestamps and notification times).
- **Archive Delay (seconds)** — how long to wait after WiFi connects before archiving starts. Default 20 is fine.
- **Snapshot Interval (seconds)** — how often the Pi snapshots the recordings the car has written. Default **900** (15 minutes) gives ~8 capture chances per segment inside GM's 2-hour rolling-delete window. Lower it for a smaller worst-case capture gap; don't raise it above roughly half the rolling window or footage can age out before it's captured.
- **Measurement System** — Metric or Imperial for temperature readouts and the temperature monitor thresholds.
- **Temperature Monitoring** — optional Warning and Caution thresholds (push-notified), a fixed log interval, and a post-archive temperature log toggle.
- **RTC Battery** (Pi 5 only) — enable the Pi 5's built-in real-time clock if you've fitted a battery on the J5 header; trickle charging is offered behind an explicit "my battery is rechargeable" acknowledgement.
- **System Tuning** — Increase Root Size (fresh installs only), Additional Packages, CPU Governor.
- **Update Source** — GitHub repo/branch used for setup downloads and OTA updates. Leave at the defaults unless you run a fork.

## 9. Review

Final summary of every choice. Click **Apply & Run Setup** to write the configuration and run setup — the Pi reboots several times during this, which is normal, and the page reconnects automatically.

Before applying, the wizard:

- **Checks free space** — if the proposed drive sizes don't fit, you get an inline error with a link to snapshot management instead of a wedged setup.
- **Detects destructive changes** — on a re-run, changing the dashcam size or the external data drive requires recreating disk images, and the wizard makes you explicitly confirm (or skip) those changes. If nothing destructive is staged, a green banner confirms your footage and snapshots are preserved.

When it finishes, the Pi comes back up at `http://dashusb.local` (or your custom hostname).
