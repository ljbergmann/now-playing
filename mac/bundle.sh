#!/usr/bin/env bash
# Builds "dist/Now Playing.app" for macOS, with its icon.
# Settings live in ~/Library/Application Support/Now Playing/settings,
# written by the setup page that opens on first start.
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

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="dist/Now Playing.app"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

cargo build --release
cargo run -q --release --example icon -- "$WORK/icon.png" 1024

ICONSET="$WORK/AppIcon.iconset"
mkdir "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s "$WORK/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$WORK/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
cp target/release/now-playing "$APP/Contents/MacOS/now-playing"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Now Playing</string>
  <key>CFBundleDisplayName</key><string>Now Playing</string>
  <key>CFBundleIdentifier</key><string>io.github.ljbergmann.now-playing</string>
  <key>CFBundleExecutable</key><string>now-playing</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature so Apple Silicon will run it (not notarized).
codesign --force --deep --sign - "$APP" >/dev/null
echo "built $APP ($VERSION)"
