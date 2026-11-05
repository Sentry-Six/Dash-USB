# Getting Started

There are two ways to install:

- **[Dash USB image](#install-with-the-dash-usb-image)** (recommended): write it with Raspberry Pi Imager, boot, done. No SSH.
- **[Manual install](#manual-install)**: for a Pi that already runs Raspberry Pi OS.

## Install with the Dash USB image

### 1. Write the image

1. Download and install [Raspberry Pi Imager](https://www.raspberrypi.com/software/) (version 2 or newer).
2. Download **[Dash-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest](https://github.com/Sentry-Six/Dash-USB/releases/latest/download/Dash-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest)** from the latest release.
3. **Double-click the downloaded file.** Raspberry Pi Imager opens with Dash USB in its list.
4. Insert your microSD card into your computer.
5. On the **Device** step, pick your Pi model. On the **OS** step, pick **Dash USB**. On the **Storage** step, pick your card.

Imager then walks you through its customisation steps. Fill them in and click **Next** (don't click **Skip customisation**):

- **Hostname**: **leave it blank.** If Imager filled in a name from last time, clear it. The Pi then appears as `dashusb.local`.
- **Localisation**: pick your nearest **capital city**. This also sets your time zone and WiFi country.
- **User**: set a **username** and **password**. Imager requires one.
- **Wi-Fi**: choose **Secure network**, then enter your WiFi name (**SSID**) and password.
- **Remote access**: optional. Turn on SSH only if you want a terminal on the Pi.

On the **Writing** step, click **Write**, then **I understand, erase and write**. Imager downloads the image and writes it.

> **Download the .rpi-imager-manifest file, not the `.img.xz` image.** The manifest is what makes Imager ask for your Wi-Fi; an image written on its own can't join your network.

### 2. Boot the Pi

1. Eject the card and put it into the Pi.
2. Power on the Pi with any USB power supply. (Later you'll move the Pi to the car and power it from the car's USB-C port.)
3. Wait about a minute (a couple of minutes on a Pi Zero 2 W or Pi 3A+). The first start sets up the card, restarts once and joins your WiFi.

### 3. Open the web UI

Open your browser and go to:

> **http://dashusb.local**

The [Setup Wizard](Setup-Wizard-Guide) will walk you through the rest. If the page doesn't load, find the Pi in your router's device list (look for **dashusb**) and open `http://<its-IP>` instead.

Then [connect to your car](#connect-to-your-car).

## Manual install

### 1. Flash the SD card

You'll use **Raspberry Pi Imager** to write Pi OS to your microSD card.

1. Download [Raspberry Pi Imager](https://www.raspberrypi.com/software/) and open it.
2. Insert your microSD card into your computer.
3. On the **Device** step, pick your Pi model, then click **Next**.
4. On the **OS** step, pick **Raspberry Pi OS (other)** → **Raspberry Pi OS Lite (64-bit)**, then click **Next**.
5. On the **Storage** step, pick your SD card, then click **Next**.

Imager then walks you through the customization steps. Fill in each one and click **Next** (don't click **Skip customisation**):

- **Hostname**: **leave it blank.** Dash USB sets its own hostname (`dashusb`) during install.
- **Localisation**: pick your nearest **capital city**. This also sets your time zone, keyboard layout and WiFi country.
- **User**: set a **username** and **password**. Write them down.
- **Wi-Fi**: choose **Secure network**, then enter your WiFi name (**SSID**) and password.
- **Remote access**: turn on **Enable SSH** → **Use password authentication**.
- Leave any other steps (such as **Raspberry Pi Connect**) as they are and click **Next**.

On the **Writing** step, click **Write**, then **I understand, erase and write**.

### 2. Boot the Pi

1. Eject the SD card from your computer and put it into the Pi.
2. Power on the Pi with any USB power supply. (Later you'll move the Pi to the car and power it from the car's USB-C port.)
3. Wait about 60 seconds for it to boot and join your WiFi.

### 3. Find the Pi's IP address

Open your router's admin page in a browser. The address is usually **http://192.168.1.1** or **http://192.168.0.1** — check the sticker on the bottom of your router.

Log in and find the device list (sometimes called "Connected Devices", "DHCP Clients", or "LAN Status"). Look for a device named **raspberrypi** and note its IP address (something like `192.168.1.47`).

### 4. SSH in and install

From your computer's terminal (Terminal on Mac, PowerShell on Windows):

```bash
ssh <your-username>@<the-IP-from-step-3>
```

Type the password you set in Pi Imager.

Once you're in, run:

```bash
sudo apt update && sudo apt upgrade -y
sudo -i
curl -fsSL https://raw.githubusercontent.com/Sentry-Six/Dash-USB/main/install-pi.sh | bash
```

> Run `apt update && apt upgrade` first to avoid stale package-index errors.

The installer downloads Dash USB, configures its services and mDNS, and renames
the Pi to `dashusb`. The SSH session may close when the hostname changes.

### 5. Open the web UI

Open your browser and go to:

> **http://dashusb.local**

The [Setup Wizard](Setup-Wizard-Guide) will walk you through the rest — picking your archive method, configuring notifications, etc.

## Connect to your car

After you finish the Setup Wizard:

1. Unplug the Pi from your power supply. Dash USB is built to lose power whenever the car shuts off, so this is safe once setup has finished.
2. Plug your USB-C data cable into **any of the car's USB-C ports**.
3. Plug the other end into the Pi.
4. The Pi boots from the car's power. The car sees a blank 64 GB FAT32 drive and creates its recording folders on it automatically — no in-car format step needed. Recording starts on its own while the vehicle is on.

Footage shows up in the web UI under **Viewer → Recordings**, organized by day.

## Need help?

- [Troubleshooting](Troubleshooting) — common install issues
- [FAQ](FAQ)
- [Discord](https://discord.gg/9QZEzVwdnt) — fastest answers
