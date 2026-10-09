# Archive Methods

Dash USB supports four archive backends. Because the vehicle removes recordings
after about two hours, the archive is the durable copy.

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

Older NAS devices may require CIFS Version `2.0` explicitly.

## rsync

For Linux/Unix servers, authenticated with an SSH key.

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

Use the wizard's connection test to confirm the configuration.

> The connection test is permissive about host keys, but scheduled archiving is
> strict. If it reports *"Host key verification failed"*, verify the server's
> fingerprint out of band before adding its key to `/root/.ssh/known_hosts`.

## rclone

For cloud storage such as Google Drive, OneDrive, Dropbox, S3, and Backblaze B2.

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
| Archive Server | `8.8.8.8` (fallback connectivity host; use the remote host for LAN storage) |

## NFS

For Linux and NAS devices that expose NFS shares.

**On your server:** export a directory in `/etc/exports` (or your NAS's GUI), allow the Pi's IP.

**In the wizard:**

| Field | Example |
|-------|---------|
| NFS Server | `192.168.1.100` |
| Export Path | `/volume1/DashUSB` (the exact path from your `exports` file) |

NFS relies on export and network controls rather than a username/password.
Restrict the export to trusted clients, or use CIFS or rsync.

## Switching methods later

You can re-run the [Setup Wizard](Setup-Wizard-Guide) from **Settings → System → Setup Wizard** to switch backends. Already-archived recordings stay where they are — Dash USB doesn't re-archive past footage, only future recordings go to the new destination.
