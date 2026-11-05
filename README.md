# now-playing

A full-screen "now playing" display for a Raspberry Pi, fed by Last.fm.
Works with anything that scrobbles to Last.fm (e.g. Tidal on any device).

- Native Rust, no browser, no desktop: draws straight to the screen via DRM/KMS
- Smooth 60 fps on a Raspberry Pi 2: page-flipped on vblank (no tearing),
  only changed pixels are copied each frame, crossfades between tracks
- Large cover art, blurred cover as background, animated "now playing" bars,
  "last played · 14 min ago" when nothing is playing
- Adapts to the attached screen: any resolution, landscape/portrait/square,
  hot-plugging, automatic or configurable rotation

## How it works

```
Last.fm ──every 6 s──▶ poller thread ──▶ compose thread ──▶ 60 fps loop ──▶ screen
                       (+ cover art)     (renders a full     (crossfade +    (DRM/KMS,
                                          static frame)       equalizer)      2 buffers)
```

The expensive work (scaling images, text) happens once per change on a
background thread. The 60 fps loop only blends finished frames (integer math,
NEON-vectorised on the Pi) and redraws the tiny equalizer region.

| Source file | What it does |
|---|---|
| `src/lastfm.rs` | Polls `user.getRecentTracks`, downloads cover art (770px) |
| `src/scene.rs` | Renders a complete frame: background, cover, text |
| `src/frame.rs` | Per-frame work: crossfade, equalizer bars |
| `src/display/kms.rs` | Pi output: double-buffered DRM/KMS with vsync page flips |
| `src/display/window.rs` | Mac output: a window, for development |
| `src/fonts.rs` | Finds Inter (or another sans) via fontconfig |

## Builds

| Board | OS | Binary |
|---|---|---|
| Raspberry Pi 2 | Raspberry Pi OS Lite **32-bit** (Bookworm or newer) | `dist/now-playing-pi2` (ARMv7, Cortex-A7 tuned) |
| Raspberry Pi 3 | Raspberry Pi OS Lite **64-bit** (Bookworm or newer) | `dist/now-playing-pi3` (AArch64, Cortex-A53 tuned) |

Build both on a Mac or Linux machine with Docker:

```bash
./pi/build.sh
```

## Install on the Pi

1. Flash Raspberry Pi OS Lite with Raspberry Pi Imager (set hostname, user,
   Wi-Fi and SSH there). The Pi 2 has no Wi-Fi: use Ethernet or a USB Wi-Fi stick.
2. Get a free Last.fm API key: https://www.last.fm/api/account/create
3. Copy and install:

```bash
scp -r dist pi <user>@<pi-name>.local:now-playing/
ssh <user>@<pi-name>.local 'bash now-playing/pi/install.sh <lastfm-username> <lastfm-api-key> && sudo reboot'
```

On the Pi: `systemctl status now-playing`, live log `journalctl -u now-playing -f`.

Or download `now-playing-<version>.tar.gz` from the
[releases](https://github.com/ljbergmann/now-playing/releases), copy it to the Pi,
and run `tar xzf now-playing-*.tar.gz && bash now-playing/pi/install.sh <user> <key>`.

## Screens

Any screen works (HDMI or DSI, any size, touch not needed). It adapts by itself:

- **Resolution:** uses the screen's preferred mode; layout and text scale to fit.
  Wide screens get cover and text side by side, portrait and square screens
  get them stacked.
- **Plug and play:** starts without a screen and waits for one; notices when the
  screen is unplugged or swapped (within ~2 s) and redraws at the new size.
- **Rotation:** panels that report how they're mounted are rotated automatically.
  For a screen mounted sideways or upside down, set `ROTATE`.
- **Two screens connected:** uses the first connected one; pick with `CONNECTOR`.
  The log shows the names (`journalctl -u now-playing`).

Set these at install time, e.g. `ROTATE=90 CONNECTOR=DSI-1 bash now-playing/pi/install.sh …`,
or edit `/etc/now-playing/env` and `sudo systemctl restart now-playing`:

| Setting | Values |
|---|---|
| `ROTATE` | `0`, `90`, `180`, `270` (clockwise), or `auto` (default) |
| `CONNECTOR` | e.g. `HDMI-A-1`, `DSI-1` (default: first connected screen) |

**Check first:** play a song in Tidal and look at your Last.fm profile. If it
shows "Scrobbling now" right away, the display follows track changes within a
few seconds. If the song only appears later, the display will lag behind.

## Develop on a Mac

```bash
cargo run --release --example mock_lastfm      # fake Last.fm on :9911
LASTFM_USER=x LASTFM_API_KEY=x LASTFM_API_URL=http://localhost:9911/2.0/ \
  cargo run --release -- --size 800x480         # window; Esc to quit
curl -X POST localhost:9911/mode/stopped       # also: playing, noart, long
```

Other flags: `--snapshot out.png` renders one frame to a file, `--bench` times
the per-frame work. Environment: `LASTFM_USER`, `LASTFM_API_KEY` (required),
`POLL_SECS` (default 6), `FONT_REGULAR` / `FONT_BOLD` (font file overrides).
