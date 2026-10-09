# Building DashUSB (Rust)

## Prerequisites

- Rust stable (1.85+, edition 2024)
- Node `^20.19.0 || >=22.12.0` and npm
- `cross` for cross-compilation: `cargo install cross`
- Docker (for `cross` and image builds)

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

64-bit (Pi 3/4/5/Zero 2):
```
cross build --release --target aarch64-unknown-linux-gnu -p sentryusb
```

32-bit (armhf — Pi 3 with 32-bit Pi OS):
```
cross build --release --target armv7-unknown-linux-gnueabihf -p sentryusb
```

Binaries land in `target/<target>/release/`.

### Native (Linux dev box)

```
cargo build --release
```

## Full OS image

`build-image.sh` wraps pi-gen with the DashUSB stage overlay:
```
./build-image.sh                  # arm64
./build-image.sh --32bit          # armhf
./build-image.sh /path/to/binary  # use a pre-built binary
```

Output: `deploy/dashusb-*.img.gz`.

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

- Rust unit tests: `cargo test`
- Frontend regression tests: `cd web && npm test`
- Shell harnesses: `test/*.sh`
- Lint: `bash check.sh` (runs ShellCheck against selected runtime scripts)

The separate **ARM dependency checks** workflow compiles and links the native
ARM64 a53 and cross-compiled ARMv7 variants when Cargo manifests, lockfiles,
Rust build scripts, Cargo configuration, or the relevant build workflows change.
It runs on pushes to `main`/`main-dev` and pull requests targeting those branches;
documentation-only changes do not start these ARM jobs. The main workflow still
runs Rust/shell tests, web tests, preview tests, lint, and the real frontend build.

Dependency checks embed a placeholder frontend and use a separate cache namespace.
They do not publish artifacts. Tags and manual Build & Release runs retain the
full four-variant build, and release publication remains tag-gated. Both ARM
workflows use the existing Ubuntu 22.04 runner labels, including the hosted
`ubuntu-22.04-arm` runner, and Ubuntu package indexes use HTTPS.

## Releasing

GitHub Releases are expected to host these artifacts (naming consumed by
`install-pi.sh` and `build-image.sh`):

- `dashusb-linux-arm64-a53` / `-a72` / `-a76` (per-CPU aarch64 variants)
- `dashusb-linux-armv7`

armv6 (Pi Zero W / Pi 1) is no longer built — the board is too underpowered
to run the daemon comfortably, and dropping the matrix entry keeps the
release artifact count manageable.
