# Building DashUSB (Rust)

## Prerequisites

- Rust stable (1.88+; some locked dependencies need it)
- Node `^20.19.0 || >=22.12.0` and npm
- `cross` for cross-compilation: `cargo install cross`
- Docker (for `cross`)

## Web UI

The web UI is React + Vite. Source lives in `web/`; the build output is embedded
into the `dashusb` binary at compile time via `rust_embed`.

```
cd web
npm ci --no-audit --no-fund
npm run build
cd ..
rm -rf crates/sentryusb/static
cp -r web/dist crates/sentryusb/static
```

## Rust binaries

One binary ships with the project:
- `dashusb` — main daemon (HTTP + WebSocket + setup orchestrator)

### Cross-compile for the Pi

64-bit (Pi 3A+/4/5/Zero 2 W):
```
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb
```

32-bit (armhf — Pi 3A+ with 32-bit Pi OS):
```
cross build --release --target armv7-unknown-linux-gnueabihf -p sentryusb
```

Binaries land in `target/<target>/release/`.

### Native (Linux dev box)

```
cargo build --release
```

## SD card image

`image/build-image.sh` takes the newest official Raspberry Pi OS Lite
(64-bit) image, installs one Dash USB release into it by running
`install-pi.sh` in image mode inside a chroot, and writes the image, its
checksum and a Raspberry Pi Imager manifest. It needs Linux, root, and an
arm64 host (or qemu-user binfmt for aarch64):

```
sudo image/build-image.sh --tag v1.3.0 --out deploy
```

The tag must be a published release: the image installs that release's
binaries and the scripts at that tag. One 64-bit image covers every
supported board (Pi 5, Pi 4, Pi 3A+, Pi Zero 2 W); the CPU picker chooses
the binary when the service starts.

The image keeps Pi OS's first boot as it ships, so the Wi-Fi, user,
hostname and SSH settings chosen in Imager apply on the user's card. Imager
2 only offers those settings for images opened through a manifest that
declares how the image is configured (`init_format`), which is why users
open the `.rpi-imager-manifest` rather than the `.img.xz` itself;
`image/imager.py` copies `init_format` and the board list from Raspberry
Pi's own entry for the base image.

Steps that depend on the board (the runtime patches, including the BLE
advertising helper for the Pi Zero 2 W) can't be decided in the chroot.
`dashusb-image-firstboot.service` runs them on the card's first boot once
GitHub is reachable, and tries again on later boots until it has.

## Deploy to an existing Pi

```
# Copy binary to the Pi and run install-pi.sh with its local path
scp target/aarch64-unknown-linux-gnu/release/dashusb pi@<ip>:/tmp/
ssh pi@<ip> sudo -i
bash install-pi.sh /tmp/dashusb
```

## Testing

### Local UI preview without a Pi

Build the real frontend with `cd web && npm ci && npm run build`, then run
`node dev/preview.mjs --port 8790 --mode after` from the repository root.
Open `http://localhost:8790`. The server binds only to loopback and serves the
actual `web/dist` application with simulated device data. Cancellation, snapshot
deletion, preferences and firmware progress change memory only; notification,
terminal, storage repair and reboot operations never touch a real device.
File uploads, folders and replacements also stay in memory (16 MiB per request,
64 MiB total). Download `upload-demo.txt` from Files and upload it again to try
the conflict/Replace flow. Restarting the preview discards these uploaded files.

To compare an older revision, build that checkout's frontend and run a second
instance with `--port 8789 --mode before --web-root /path/to/older/web/dist`.
The before and after servers keep independent state. Restart a server to reset
its sample device, or POST `/__preview/reset` to start another sample archive.

### Automated checks

- Rust unit tests: `cargo test --workspace --locked`
- Clippy: `cargo +1.99.0 clippy --workspace --all-targets --locked -- -D warnings` (CI pins Rust 1.99.0)
- Frontend regression tests: `cd web && npm test`
- Shell harnesses: `test/*.sh`
- Lint: `bash check.sh` (runs ShellCheck against selected runtime scripts)

The separate **ARM dependency checks** workflow compiles and links the native
ARM64 a53 and cross-compiled ARMv7 variants when Cargo manifests, lockfiles,
Rust build scripts, Cargo configuration, or the relevant build workflows change.
It runs on pushes to `main` and pull requests targeting it;
documentation-only changes do not start these ARM jobs. The main workflow still
runs Rust/shell tests, web tests, preview tests, lint, and the real frontend build.

Dependency checks embed a placeholder frontend and use a separate cache namespace.
They do not publish artifacts. Tags and manual Build & Release runs retain the
full four-variant build, and release publication remains tag-gated. Both ARM
workflows use the existing Ubuntu 22.04 runner labels, including the hosted
`ubuntu-22.04-arm` runner, and Ubuntu package indexes use HTTPS.

## Releasing

GitHub Releases are expected to host these artifacts (naming consumed by
`install-pi.sh` and the updater):

- `dashusb-linux-arm64-a53` / `-a72` / `-a76` (per-CPU aarch64 variants)
- `dashusb-linux-armv7`

Pushing a `v*` tag runs **Build & Release**, which publishes the binaries.
When it succeeds, the **SD Card Image** workflow builds the image for that
tag and attaches:

- `Dash-USB-for-Raspberry-Pi-Imager.rpi-imager-manifest` (what users open)
- `dashusb-raspberry-pi-image.img.xz` and `dashusb-raspberry-pi-image.img.xz.sha256`
- `dashusb-imager-os-list.json` (the entry Raspberry Pi's own Imager list
  would read)

It also adds one line pointing at the manifest to the top of the release
notes. Automatic image builds run only for stable releases, and only while
the repository variable `DASHUSB_SD_IMAGE` is `auto`; a pre-release promoted
to a full release gets its image then. To build by hand, run **SD Card
Image** from the Actions tab with the tag. Turn off **publish** to keep the
image as a workflow artifact for 7 days instead of attaching it, or turn
on **replace** to rebuild an image a release already has.

armv6 (Pi Zero W / Pi 1) is no longer built — the board is too underpowered
to run the daemon comfortably, and dropping the matrix entry keeps the
release artifact count manageable.
