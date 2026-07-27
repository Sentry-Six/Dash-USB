# Archive Methods

Pick whichever fits how you already store stuff. Dash USB archives every recording the car writes to one of four backends — and because GM's firmware deletes everything older than about 2 hours from the drive, the archive is the only durable copy of your footage.

## CIFS / SMB

For Windows file sharing, macOS file sharing, and most consumer NAS devices (Synology, QNAP, TrueNAS).

**On your server:** create a shared folder, give a user read/write access, note the share name.

**In the wizard:**

| Field | Example |
|-------|---------|
| Archive Server | `192.168.1.100` or `nas.local` |
| Share Name | `DashUSB` or `media/dashcam` |
| Username | `dashcam` |
| Password | _your password_ |
| Domain | leave blank unless your server needs it |
| CIFS Version | leave blank unless you know you need 2.0 / 1.0 |

If the connection fails, the most common cause is an older NAS that needs the CIFS Version set to `2.0` explicitly.

## rsync

For Linux/Unix servers. Faster and more reliable than CIFS over the open internet, and authenticated with an SSH key instead of a password.

**On your server:** create a user, create a destination folder.

**In the wizard:**

| Field | Example |
|-------|---------|
| Server | `archive.example.com` |
| Username | `dashcam` |
| Remote Path | `/home/dashcam/dashcam` |

The wizard's rsync section also **generates an SSH key for the Pi** and shows the public key with a copy button. On your server, run:

```bash
mkdir -p ~/.ssh && chmod 700 ~/.ssh
echo "<paste-key-here>" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

Then use the wizard's connection test to confirm it works. You only have to do this once.

> The connection test is permissive about host keys, but the real archive job uses strict host-key checking. If archiving later fails with *"Host key verification failed"*, SSH into the Pi and run `ssh-keyscan -H your-server >> /root/.ssh/known_hosts`.

## rclone

For cloud storage — Google Drive, OneDrive, Dropbox, S3, Backblaze B2, and ~60 other providers. Best for offsite backups.

**Set up the remote first** by SSH'ing into the Pi and running:

```bash
sudo rclone config
```

Follow the prompts — it'll ask which cloud service, walk you through OAuth, and let you name the remote (e.g., `gdrive`).

**In the wizard:**

| Field | Example |
|-------|---------|
| Remote Name | `gdrive` (matches the name you set in `rclone config`) |
| Remote Path | `Dashcam` or `Backups/DashUSB` |
| Archive Server | `8.8.8.8` (a host to ping for the connectivity check — use your server's IP if the remote is on your LAN) |

## NFS

For Linux NAS devices that prefer NFS over CIFS (some Synology and TrueNAS setups). **Typically faster than CIFS / SMB** on the same hardware — worth using if you have a lot of footage to back up and your NAS supports it.

**On your server:** export a directory in `/etc/exports` (or your NAS's GUI), allow the Pi's IP.

**In the wizard:**

| Field | Example |
|-------|---------|
| NFS Server | `192.168.1.100` |
| Export Path | `/volume1/DashUSB` (the exact path from your `exports` file) |

NFS is unauthenticated — anyone on your LAN with the path can read/write. Use CIFS or rsync if that's a concern.

## Switching methods later

You can re-run the [Setup Wizard](Setup-Wizard-Guide) from **Settings → System → Setup Wizard** to switch backends. Already-archived recordings stay where they are — Dash USB doesn't re-archive past footage, only future recordings go to the new destination.
