#!/usr/bin/env bash
# Builds dist/now-playing-windows/ (Now Playing.exe + WebView2Loader.dll, 64-bit Windows 10/11) on a Mac or Linux
# machine with MinGW-w64 (macOS: `brew install mingw-w64`) and rustup.
# Untested on real Windows so far.
set -euo pipefail
cd "$(dirname "$0")/.."

# The app's Last.fm key, as in pi/build.sh (never in the repo).
BUILD_ENV="${NP_BUILD_ENV:-$HOME/.config/now-playing/build.env}"
LASTFM_APP_KEY=""
LASTFM_APP_SECRET=""
if [[ -f "$BUILD_ENV" ]]; then
  LASTFM_APP_KEY=$(sed -n 's/^LASTFM_APP_KEY=//p' "$BUILD_ENV" | head -1)
  LASTFM_APP_SECRET=$(sed -n 's/^LASTFM_APP_SECRET=//p' "$BUILD_ENV" | head -1)
fi
export LASTFM_APP_KEY LASTFM_APP_SECRET
echo "Last.fm app key: $([[ -n $LASTFM_APP_KEY ]] && echo built in || echo none, users enter their own)"

rustup target add x86_64-pc-windows-gnu >/dev/null
# rustup's compiler (it has the Windows target), not a system one.
RUSTC_WRAPPER= RUSTC="$(rustup which rustc)" CARGO_TARGET_DIR=target/win \
  cargo build --release --locked --target x86_64-pc-windows-gnu

# The exe needs Microsoft's WebView2Loader.dll next to it (for the
# VinylVault sign-in window); webview2-com-sys ships it, redistributable.
LOADER=$(cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-gnu 2>/dev/null \
  | python3 -c 'import json,sys,os; d=json.load(sys.stdin); p=[x["manifest_path"] for x in d["packages"] if x["name"]=="webview2-com-sys"][0]; print(os.path.join(os.path.dirname(p),"x64","WebView2Loader.dll"))')
OUT=dist/now-playing-windows
rm -rf "$OUT" dist/now-playing-windows.exe
mkdir -p "$OUT"
cp target/win/x86_64-pc-windows-gnu/release/now-playing.exe "$OUT/Now Playing.exe"
cp "$LOADER" "$OUT/WebView2Loader.dll"
ls -lh "$OUT"
