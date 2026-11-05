#!/usr/bin/env bash
# Builds both Pi binaries inside a Debian (bookworm) container:
#   dist/now-playing-pi2   32-bit ARMv7, tuned for the Pi 2 (Cortex-A7, NEON)
#   dist/now-playing-pi3   64-bit ARM,   tuned for the Pi 3 (Cortex-A53)
# Needs Docker. On an Apple Silicon Mac the container runs natively (arm64).
# Without the desktop window: a Pi with OS Lite draws straight to the screen.
set -euo pipefail
cd "$(dirname "$0")/.."

# The app's Last.fm key (so users only need their user name). Kept outside
# the repo; builds without it ask users for their own key.
BUILD_ENV="${NP_BUILD_ENV:-$HOME/.config/now-playing/build.env}"
# With the secret too, the setup page offers "Connect with Last.fm".
LASTFM_APP_KEY=""
LASTFM_APP_SECRET=""
if [[ -f "$BUILD_ENV" ]]; then
  LASTFM_APP_KEY=$(sed -n 's/^LASTFM_APP_KEY=//p' "$BUILD_ENV" | head -1)
  LASTFM_APP_SECRET=$(sed -n 's/^LASTFM_APP_SECRET=//p' "$BUILD_ENV" | head -1)
fi
export LASTFM_APP_KEY LASTFM_APP_SECRET
echo "Last.fm app key: $([[ -n $LASTFM_APP_KEY ]] && echo built in || echo none, users enter their own)$([[ -n $LASTFM_APP_SECRET ]] && echo ', with Connect with Last.fm')"

docker run --rm \
  -e LASTFM_APP_KEY -e LASTFM_APP_SECRET \
  -v "$PWD":/src -w /src \
  -v now-playing-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/src/target/pi \
  rust:1-bookworm bash -euo pipefail -c '
    apt-get update -qq
    apt-get install -y -qq gcc-arm-linux-gnueabihf gcc-aarch64-linux-gnu >/dev/null
    rustup target add armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu >/dev/null

    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
    export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C target-cpu=cortex-a7"
    cargo build --release --locked --no-default-features --target armv7-unknown-linux-gnueabihf

    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=cortex-a53"
    cargo build --release --locked --no-default-features --target aarch64-unknown-linux-gnu
  '

mkdir -p dist
cp target/pi/armv7-unknown-linux-gnueabihf/release/now-playing dist/now-playing-pi2
cp target/pi/aarch64-unknown-linux-gnu/release/now-playing dist/now-playing-pi3
ls -lh dist/
