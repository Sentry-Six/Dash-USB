# Dash USB Wiki

Turn a Raspberry Pi into a smart USB drive for your GM vehicle's built-in
dashcam (Surround Vision Recorder). The car limits footage to a rolling
**2 hours** no matter how big the drive is — Dash USB snapshots the
recordings before the car deletes them and archives everything to your
own server. A web UI lets you review and back up footage — no SSH, no
config files.

## Install over SSH

> **No prebuilt SD card image yet.** Flash **Raspberry Pi OS Lite (64-bit)**,
> connect over SSH, and follow [Getting Started](Getting-Started).

On a Pi already running Pi OS:

```bash
sudo apt update && sudo apt upgrade -y
sudo -i
curl -fsSL https://raw.githubusercontent.com/Sentry-Six/Dash-USB/main/install-pi.sh | bash
```

> Refresh and upgrade packages first to avoid stale package-index errors.

Then open `http://dashusb.local` and the [Setup Wizard](Setup-Wizard-Guide) takes you the rest of the way.

## What you need

| Tier | Boards | Notes |
|------|--------|-------|
| **Recommended** | Raspberry Pi 4B, Raspberry Pi 5 | USB 2.0 OTG — fastest archiving, best web UI responsiveness |
| **Should work** | Raspberry Pi Zero 2 W, Raspberry Pi 3 Model A+ | USB 2.0 OTG, slower archive speeds |

> **The Pi 3 Model B and B+ will not work.** Their single USB channel runs through an
> onboard hub chip that provides Ethernet and the four USB-A ports, which leaves the
> micro-USB port power-only with no OTG support. The 3A+ omits that chip, so its
> micro-USB port is wired straight to the SoC and can act as a USB gadget.

Plus:
- A **high-endurance MicroSD card, 256 GB or larger** (512 GB recommended —
  GM records roughly 5 GB per hour of driving, all of which Dash USB keeps
  until space runs out)
- A **USB-C data cable** (not charge-only) — Pi to any of the car's USB-C ports
- Wi-Fi; internet access is required for initial setup, updates, and cloud archiving

> Your vehicle needs GM's **Surround Vision Recorder** feature (the built-in
> rolling dashcam on newer GM EVs and ICE vehicles). Confirmed working on a
> 2026 model — the car accepts the Pi's USB 2.0 gadget even though GM's spec
> sheet says USB 3.0.

## Quick Links

| Page | What's on it |
|------|--------------|
| [Getting Started](Getting-Started) | Installation walkthrough |
| [Setup Wizard Guide](Setup-Wizard-Guide) | Every wizard step explained |
| [Archive Methods](Archive-Methods) | CIFS, rsync, rclone, NFS |
| [Notifications](Notifications) | Push notifications to your phone |
| [Privacy](Privacy) | Outbound data and privacy controls |
| [Troubleshooting](Troubleshooting) | Things that go wrong |
| [FAQ](FAQ) | Common questions |

## Links

- **Site**: [sentry-six.com](https://sentry-six.com)
- **GitHub**: [Sentry-Six/Dash-USB](https://github.com/Sentry-Six/Dash-USB)
- **Releases**: [Latest](https://github.com/Sentry-Six/Dash-USB/releases/latest)
- **Discord**: [Community chat](https://discord.gg/9QZEzVwdnt)
- **License**: PolyForm Noncommercial 1.0.0 (source-available; free for noncommercial use)

Dash USB is the GM sibling of the Tesla project [Sentry USB](https://github.com/Sentry-Six/Sentry-USB-Rusty),
from the Sentry Six project. Not affiliated with General Motors.
