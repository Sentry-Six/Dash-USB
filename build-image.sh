#!/bin/bash -eu
#
# Build a DashUSB (Rust) Raspberry Pi image locally using pi-gen + Docker.
#
# Prerequisites:
#   - Docker installed and running
#   - Internet access (to download Raspberry Pi OS base)
#   - For local builds: cargo + cross (cargo install cross)
#
# Usage:
#   ./build-image.sh                         # 64-bit image (Pi 3/4/5/Zero 2)
#   ./build-image.sh --32bit                 # 32-bit image (armhf — Pi 3 with 32-bit Pi OS)
#   ./build-image.sh /path/to/binary         # 64-bit with local binary
#   ./build-image.sh --32bit /path/to/binary # 32-bit with local binary
#
# Note: the original armv6 Pi Zero W / Pi 1 are no longer supported; DashUSB
# requires Pi Zero 2 W or newer.
#
# Output:
#   deploy/dashusb-*.img.gz — ready to flash with Raspberry Pi Imager
#

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="/tmp/dashusb/pi-gen"
REPO="Sentry-Six/Dash-USB"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

info()  { echo -e "${BLUE}[INFO]${NC} $1"; }
ok()    { echo -e "${GREEN}[OK]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# ── Parse arguments ──
BUILD_32BIT=false
LOCAL_BINARY=""
for arg in "$@"; do
    case "$arg" in
        --32bit|--32|--armhf|--pizero)
            BUILD_32BIT=true
            ;;
        *)
            if [ -f "$arg" ]; then
                LOCAL_BINARY="$(cd "$(dirname "$arg")" && pwd)/$(basename "$arg")"
            fi
            ;;
    esac
done

if $BUILD_32BIT; then
    ARCH_LABEL="32-bit (armhf — Pi 3 with 32-bit Pi OS)"
    # 32-bit: single binary; SUFFIXES list has one entry to keep the
    # loop logic below uniform with the 64-bit path.
    SUFFIXES=("linux-armv7")
    CPUS=("cortex-a7")
    RUST_TARGET="armv7-unknown-linux-gnueabihf"
    # Tesla vehicle-command is Go; cross-compile targets map independently
    # from our Rust targets. GOARM=7 matches the Rust target (cortex-a7);
    # the upstream armv6 tarball stays compatible for any board, but Go's
    # GOARM=7 produces faster code on the targets we still support.
    GO_ARCH="arm"
    GO_ARM="7"
    CONFIG_FILE="pi-gen-config-32bit"
else
    ARCH_LABEL="64-bit (arm64 — Pi 3/4/5/Zero 2)"
    # 64-bit: three per-CPU-tuned variants. The runtime picker selects
    # the right one at every service start.
    SUFFIXES=("linux-arm64-a53" "linux-arm64-a72" "linux-arm64-a76")
    CPUS=("cortex-a53" "cortex-a72" "cortex-a76")
    RUST_TARGET="aarch64-unknown-linux-gnu"
    GO_ARCH="arm64"
    GO_ARM=""
    CONFIG_FILE="pi-gen-config"
fi

info "Building $ARCH_LABEL image"

# Check prerequisites
command -v docker &>/dev/null || error "Docker is required. Install it first."

# ── Step 1: Get the DashUSB binary variants ──
#
# Populates three parallel arrays:
#   VARIANT_PATHS[i]   — local path to the dashusb binary for SUFFIXES[i]
# These get injected into pi-gen's stage_dashusb/files/ in Step 4.
VARIANT_PATHS=()

if [ -n "$LOCAL_BINARY" ]; then
    # Local-binary mode: one binary on the CLI, stage it under all variants.
    # The picker fallback chain handles boards that would prefer a more
    # specific variant — they just fall back to whatever's actually there.
    info "Using local binary: $LOCAL_BINARY (staged under all ${#SUFFIXES[@]} variant slot(s))"
    for sfx in "${SUFFIXES[@]}"; do
        VARIANT_PATHS+=("$LOCAL_BINARY")
    done
elif command -v cross &>/dev/null && command -v node &>/dev/null; then
    info "Building binaries from source (${#SUFFIXES[@]} variant(s))..."
    (
        cd "$SCRIPT_DIR/web"
        npm ci --no-audit --no-fund 2>&1 | tail -3
        npm run build 2>&1 | tail -3
        rm -rf "$SCRIPT_DIR/crates/sentryusb/static"
        mkdir -p "$SCRIPT_DIR/crates/sentryusb/static"
        cp -r dist/. "$SCRIPT_DIR/crates/sentryusb/static/"
    )
    # Cross-compile once per CPU variant. RUSTFLAGS overrides the
    # per-target-cpu setting in .cargo/config.toml; the target/ subdir
    # used by cargo is keyed only by triple, so we move the output to a
    # per-CPU stash to avoid clobbering between iterations.
    for i in "${!SUFFIXES[@]}"; do
        sfx="${SUFFIXES[$i]}"
        cpu="${CPUS[$i]}"
        info "  → building ${sfx} (target-cpu=${cpu})..."
        (
            cd "$SCRIPT_DIR"
            cargo clean --release --target "$RUST_TARGET" -p sentryusb 2>/dev/null || true
            # a53/a72 (BCM2710/2711) lack the ARMv8 crypto extension; LLVM's
            # CPU defs assume it, and RustCrypto skips runtime detection when
            # the feature is compile-time on — baked-in AES/SHA SIGILLs on
            # those boards. Mirror build.yml: strip crypto except on a76.
            case "$cpu" in
                cortex-a76) FEATURES="" ;;
                *)          FEATURES=" -C target-feature=-aes,-sha2" ;;
            esac
            RUSTFLAGS="-C target-cpu=${cpu}${FEATURES}" cross build --release --target "$RUST_TARGET" -p sentryusb
        )
        STASH="/tmp/dashusb-image-build/${sfx}"
        mkdir -p "$STASH"
        cp "$SCRIPT_DIR/target/$RUST_TARGET/release/dashusb" "$STASH/dashusb"
        VARIANT_PATHS+=("$STASH/dashusb")
    done
    ok "Built ${#SUFFIXES[@]} variant(s)"
else
    info "cross/Node not available locally. Downloading from GitHub releases..."
    for sfx in "${SUFFIXES[@]}"; do
        STASH="/tmp/dashusb-image-build/${sfx}"
        mkdir -p "$STASH"
        curl -fsSL "https://github.com/$REPO/releases/latest/download/dashusb-$sfx" -o "$STASH/dashusb" \
            || error "Failed to download dashusb-$sfx. Build locally with:\n  cargo install cross\n  cd web && npm ci && npm run build"
        VARIANT_PATHS+=("$STASH/dashusb")
    done
    ok "Downloaded ${#SUFFIXES[@]} variant(s)"
fi

# Sanity: at least one main dashusb binary must exist.
for p in "${VARIANT_PATHS[@]}"; do
    [ -f "$p" ] || error "Missing dashusb variant at $p"
done

# ── Step 2: Clone pi-gen ──
info "Setting up pi-gen..."
rm -rf "$WORK_DIR"
if $BUILD_32BIT; then
    git clone --depth 1 https://github.com/RPi-Distro/pi-gen.git "$WORK_DIR"
else
    git clone --depth 1 --branch arm64 https://github.com/RPi-Distro/pi-gen.git "$WORK_DIR"
fi

# ── Step 3: Prepare pi-gen with DashUSB config ──
cd "$WORK_DIR"
bash "$SCRIPT_DIR/pi-gen-sources/prepare.sh"

cp "$SCRIPT_DIR/pi-gen-sources/$CONFIG_FILE" "$WORK_DIR/config"

# ── Step 4: Inject the pre-built binaries and BLE daemon ──
info "Injecting DashUSB binary variants into image build..."
STAGE_FILES="$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files"
for i in "${!SUFFIXES[@]}"; do
    sfx="${SUFFIXES[$i]}"
    cp "${VARIANT_PATHS[$i]}" "$STAGE_FILES/dashusb-${sfx}"
    chmod +x "$STAGE_FILES/dashusb-${sfx}"
    info "  → staged dashusb-${sfx}"
done

# Picker script: the runtime selector that decides which variant runs.
cp "$SCRIPT_DIR/pi-gen-sources/00-dashusb-tweaks/files/dashusb-pick-binary" \
    "$STAGE_FILES/dashusb-pick-binary"
chmod +x "$STAGE_FILES/dashusb-pick-binary"

info "Injecting BLE daemon files..."
cp "$SCRIPT_DIR/server/ble/dashusb-ble.py" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/dashusb-ble.py" 2>/dev/null \
    || cp "$SCRIPT_DIR/../Sentry-USB/server/ble/dashusb-ble.py" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/dashusb-ble.py"
cp "$SCRIPT_DIR/server/ble/dashusb-ble.service" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/dashusb-ble.service" 2>/dev/null \
    || cp "$SCRIPT_DIR/../Sentry-USB/server/ble/dashusb-ble.service" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/dashusb-ble.service"
cp "$SCRIPT_DIR/server/ble/com.dashusb.ble.conf" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/com.dashusb.ble.conf" 2>/dev/null \
    || cp "$SCRIPT_DIR/../Sentry-USB/server/ble/com.dashusb.ble.conf" "$WORK_DIR/stage_dashusb/00-dashusb-tweaks/files/com.dashusb.ble.conf"

# Trixie apt indices are much larger; increase export image margin
if [[ "$OSTYPE" == darwin* ]]; then
    sed -i '' 's/200 \* 1024 \* 1024/800 * 1024 * 1024/' "$WORK_DIR/export-image/prerun.sh"
else
    sed -i 's/200 \* 1024 \* 1024/800 * 1024 * 1024/' "$WORK_DIR/export-image/prerun.sh"
fi

# ── Step 5: Build the image ──
info "Building image with Docker (this takes 15-30 minutes)..."
./build-docker.sh

# ── Step 6: Copy output ──
IMAGE=$(find "$WORK_DIR/deploy" -name '*.img' | head -1)
if [ -z "$IMAGE" ]; then
    error "Build failed — no image found in deploy/"
fi

mkdir -p "$SCRIPT_DIR/deploy"
info "Compressing image..."
gzip -9 -c "$IMAGE" > "$SCRIPT_DIR/deploy/$(basename "$IMAGE").gz"

ok "Image built successfully!"
echo ""
echo -e "  ${GREEN}Output:${NC} $SCRIPT_DIR/deploy/$(basename "$IMAGE").gz"
echo -e "  ${GREEN}Arch:${NC}   $ARCH_LABEL"
echo ""
echo "  Flash with Raspberry Pi Imager:"
echo "    1. Select 'Use custom' → choose the .img.gz file"
echo "    2. Configure WiFi, hostname (dashusb), SSH, password in settings"
echo "    3. Write to SD card"
echo ""
echo "  After first boot, open http://dashusb.local in your browser."
echo ""

# Cleanup
rm -rf "$WORK_DIR"
