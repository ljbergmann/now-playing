#!/usr/bin/env bash
# Installs now-playing on a Raspberry Pi running Raspberry Pi OS Lite (no desktop).
#   Pi 2  → 32-bit OS, uses dist/now-playing-pi2
#   Pi 3  → 64-bit OS, uses dist/now-playing-pi3 (or 32-bit OS with the pi2 build)
#
# From the Mac:   scp -r dist pi <user>@<pi-name>.local:now-playing/
# On the Pi:      bash now-playing/pi/install.sh
#
# Then scan the QR code on the screen to connect Last.fm and pick a look.
# Safe to re-run, e.g. to update the binary.
set -euo pipefail

STATE=/var/lib/now-playing

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "$(uname -m)" in
  armv7l|armv6l) BIN="$HERE/dist/now-playing-pi2" ;;
  aarch64)       BIN="$HERE/dist/now-playing-pi3" ;;
  *) echo "unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
[[ -f "$BIN" ]] || { echo "missing $BIN (run pi/build.sh on the Mac and copy dist/)" >&2; exit 1; }

echo "==> Installing fonts"
sudo apt-get update -qq
sudo apt-get install -y -qq fontconfig fonts-inter

echo "==> Installing $(basename "$BIN") to /usr/local/bin/now-playing"
sudo install -m 755 "$BIN" /usr/local/bin/now-playing

echo "==> Settings"
# The setup page writes $STATE/settings. Older installs kept the account in
# /etc/now-playing/env, where the setup page couldn't change it: move it over once.
sudo install -d -o "$USER" -m 700 "$STATE"
if sudo test -f /etc/now-playing/env && sudo grep -q '^LASTFM_USER=' /etc/now-playing/env && ! sudo test -f "$STATE/settings"; then
  sudo install -o "$USER" -m 600 /dev/null "$STATE/settings"
  sudo grep -E '^(LASTFM_USER|LASTFM_API_KEY|THEME|CLOCK|VINYLVAULT_SHELF)=' /etc/now-playing/env | sudo tee "$STATE/settings" >/dev/null
fi
# Only fixed, screen-related options go into the service environment
# (values there can't be changed from the setup page).
sudo install -d -m 700 /etc/now-playing
sudo install -m 600 /dev/null /etc/now-playing/env
sudo tee /etc/now-playing/env >/dev/null <<EOF
${VINYLVAULT_CATALOGUE:+VINYLVAULT_CATALOGUE=$VINYLVAULT_CATALOGUE}
${PROMO:+PROMO=$PROMO}
${STATS:+STATS=$STATS}
${ROTATE:+ROTATE=$ROTATE}
${CONNECTOR:+CONNECTOR=$CONNECTOR}
EOF

echo "==> Installing the service"
sudo tee /etc/systemd/system/now-playing.service >/dev/null <<EOF
[Unit]
Description=Now Playing display (Last.fm)
After=network-online.target systemd-user-sessions.service
Wants=network-online.target

[Service]
User=$USER
SupplementaryGroups=video render
EnvironmentFile=/etc/now-playing/env
# Settings written by the setup page: /var/lib/now-playing/settings
StateDirectory=now-playing
StateDirectoryMode=0700
ExecStart=/usr/local/bin/now-playing
Restart=always
RestartSec=3
# Smooth animation beats background chores.
Nice=-5

[Install]
WantedBy=multi-user.target
EOF

# No login prompt / blinking cursor / console blanking behind the display.
sudo systemctl disable --now getty@tty1.service 2>/dev/null || true
CMDLINE=/boot/firmware/cmdline.txt
[[ -f $CMDLINE ]] || CMDLINE=/boot/cmdline.txt
for opt in consoleblank=0 vt.global_cursor_default=0 logo.nologo; do
  grep -qw "$opt" "$CMDLINE" || sudo sed -i "1 s/\$/ $opt/" "$CMDLINE"
done

# The display needs the KMS graphics driver (default on current Pi OS).
CONFIG=/boot/firmware/config.txt
[[ -f $CONFIG ]] || CONFIG=/boot/config.txt
if ! grep -q '^dtoverlay=vc4-kms-v3d' "$CONFIG"; then
  echo "dtoverlay=vc4-kms-v3d" | sudo tee -a "$CONFIG" >/dev/null
  echo "    (enabled the vc4-kms-v3d graphics driver)"
fi

sudo systemctl daemon-reload
sudo systemctl enable now-playing.service
sudo systemctl restart now-playing.service

echo "==> Done. Scan the QR code on the screen to finish setting up."
echo "    Open the setup page again later: sudo systemctl kill -s USR1 now-playing"
echo "    Status: systemctl status now-playing   Log: journalctl -u now-playing -f"
echo "    Reboot once so the console settings apply: sudo reboot"
