# Troubleshooting

Most install problems fall into a few buckets. If your issue isn't here, ping **[Discord](https://discord.gg/9QZEzVwdnt)** — answers come fast and we'll add common ones to this page over time.

## Can't reach http://dashusb.local

The Pi is up but its hostname isn't resolving on your network.

**Try:**
1. Open your router's admin page and confirm the Pi shows up as `dashusb`. Use the IP directly in your browser instead (e.g., `http://192.168.1.47`).
2. On Windows, install **[Bonjour Print Services](https://support.apple.com/kb/DL999)** — Windows doesn't ship with mDNS by default.
3. Some corporate / mesh / guest WiFi networks block mDNS broadcasts. Try a different network or use the IP.

## The car never starts recording to the Pi

The car isn't seeing the Pi as a USB drive.

**Try:**
1. **Check the cable** — make sure it's a **USB-C data cable**, not charge-only. The cheap ones bundled with most USB chargers are charge-only.
2. On a **Pi Zero 2 W**, connect the car's cable to the port labelled **USB**, not **PWR IN**. The `PWR IN` port can power the Pi, but it does not carry the USB data connection the car needs.
3. Try a different USB-C port in the car — any of them should work.
4. Make sure the vehicle is **on**. The Surround Vision Recorder only records while the vehicle is on/driving, not while parked and locked.
5. Power-cycle the Pi (unplug, wait 5 seconds, plug back in).
6. SSH into the Pi and check the main service: `systemctl status dashusb`. It includes the USB-gadget functionality; there is no separate gadget systemd unit.

Notes that save head-scratching:

- The car **auto-creates its folder tree** on the blank drive — there's no in-car format step, and no folders to create yourself.
- GM requires a drive of **≥64 GB with ≥32 GB available** or it refuses to record. Dash USB presents a 64 GB FAT32 virtual drive and automatically keeps 32 GiB free on it, so this should never be the cause — but if you edited `CAM_SIZE` below 64G by hand, that's why.
- The car spec says USB 3.0, but a Pi's USB 2.0 gadget is accepted fine (confirmed on a 2026 model).

## Footage older than 2 hours is gone from the drive

That's not a bug in Dash USB — it's GM's firmware. The car hard-deletes recordings older than a rolling ~2 hours from **any** USB drive, no matter how large.

Dash USB's whole job is to defeat this: it snapshots the recordings every 15 minutes (before the rolling delete reaches them) and archives them to your server. Your preserved footage is in the web UI under **Viewer → Recordings** (organized by day) and on your archive server — not on the virtual drive the car sees.

If footage is missing from the Viewer too:
1. Check the **Snapshots** page — are snapshots being taken?
2. Check **Logs** for archive errors, and `journalctl -u dashusb-archive -n 100` over SSH.
3. Make sure the Snapshot Interval hasn't been raised far above the default 900 seconds ([Setup Wizard → Advanced](Setup-Wizard-Guide#8-advanced)).

## There's a tiny ~6 MB clip at the end of my drive

Normal. The car writes a short partial segment when you switch the vehicle off mid-segment. It's archived like any other clip.

## CIFS/SMB connection fails

**Try:**
1. Re-run the [Setup Wizard](Setup-Wizard-Guide) → Archive step → fill in **CIFS Version** as `2.0` or `1.0` (some older NAS devices reject SMB3 negotiation).
2. Check your password — special characters sometimes need escaping; try a simpler password as a test.
3. From the Pi, try mounting manually:
   ```bash
   sudo mount -t cifs //<server>/<share> /mnt -o username=<user>
   ```
   If that errors, the error message tells you exactly what's wrong.

## Still stuck?

- **[Discord](https://discord.gg/9QZEzVwdnt)** — fastest help, real humans
- **[Open an issue](https://github.com/Sentry-Six/Dash-USB/issues)** — for reproducible bugs
- Include: Pi model, vehicle model/year, what step you're stuck on, exact error message, and the output of `journalctl -u dashusb -n 100` if relevant.
