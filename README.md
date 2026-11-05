# now-playing

A "now playing" display fed by Last.fm, for whatever hardware and screen you
have: a Raspberry Pi with any monitor, or an app on Mac, Windows and Linux (in a
window or fullscreen on any monitor). Works with anything that scrobbles to
Last.fm, e.g. Tidal on any device.

- **Easy setup:** on a Pi, scan the QR code on the screen with your phone; on a
  computer, the app's own settings window. No API key, no config files.
- **Three looks** for next to a record player, with a big clock when nothing plays.
- **Your own pressings:** with VinylVault, the record on screen is the coloured
  vinyl you actually own.
- **Native Rust, 60 fps on a Raspberry Pi 2:** draws straight to the screen
  (DRM/KMS, vsync, no browser), adapts to any screen size and orientation.

## Setup

**On a computer** (see [Desktop app](#desktop-app)): the settings window opens
on first start. Type your Last.fm user name, optionally **Connect with
VinylVault** (the system's sign-in sheet), pick a look. No browser involved.

**On a Pi:**

1. Install it (see [Install on the Pi](#install-on-the-pi)).
2. The screen shows a QR code. Scan it with your phone. A small page opens,
   served by the display itself on your home network.
3. **Last.fm:** tap **Connect with Last.fm** and **Allow** (or type your user name).
4. **VinylVault (optional):** tap **Connect with VinylVault**, sign in, **Allow**
   (or paste a shared shelf link).
5. **Look:** tap Vinyl, Poster or Ambient and pick the clock format; the display
   changes as you tap. Then **Done**.

The page only works with the random code in the QR address and closes after
"Done" or 20 idle minutes. Open it again later with
`sudo systemctl kill -s USR1 now-playing`.

The display stores only your Last.fm user name (no Last.fm login pass) and, if
connected, a read-only VinylVault pass that works for nothing but your shelf.
Disconnect a display anytime at https://vinylvault.sky-lab.de/connect.

## Looks

| Look | Playing | Nothing playing |
|---|---|---|
| Vinyl (default) | A record with the cover as its label, spinning at 33⅓ rpm, tonearm in the groove | Arm lifts, record spins down, big clock with the last track |
| Poster | Cover fills the screen, heavy type over a dark fade, accent colour from the art | Cover goes dark and grey, huge clock |
| Ambient | Cover floating on a slowly drifting colour field made from its own colours | Thin clock in the middle, last cover as a small thumbnail |

Under the playing track: your Last.fm numbers, e.g. "♥ · 38th play · 1,204 plays
of Ariana Grande". Tracks without cover art get generated art.

### Your own pressings (VinylVault)

Connected to [VinylVault](https://vinylvault.sky-lab.de), the vinyl look draws the
record as **the pressing you own** whenever an album from your shelf plays:
marbled, splatter, swirl, galaxy, picture disc, half & half, clear, glow or solid
colour, with a "YOUR PRESSING · RED MARBLED · LTD. 500" line. Albums you own on
plain black stay black. The shelf refreshes every 15 minutes; matching ignores
"(Deluxe Edition)", " - Single" and featured artists.

Albums you don't own get a real pressing of that album from the public VinylVault
catalogue ("ON VINYL · MOONSTONE BLUE MARBLED"), the same one every time.

## Screens

Any HDMI or DSI screen works; touch isn't needed. 4:3 (e.g. 8", 1024×768) suits
covers and records better than 16:9. The display adapts by itself:

- **Resolution:** uses the screen's preferred mode; layout and text scale to fit.
  Wide screens get cover and text side by side, portrait and square stacked.
- **Plug and play:** starts without a screen and waits; notices an unplugged or
  swapped screen within ~2 s and redraws at the new size.
- **Rotation:** panels that report how they're mounted are rotated automatically;
  otherwise set `ROTATE`.
- **Two screens:** uses the first connected one; pick with `CONNECTOR`
  (`journalctl -u now-playing` shows the names).

## Install on the Pi

| Board | OS | Binary |
|---|---|---|
| Raspberry Pi 2 | Raspberry Pi OS Lite **32-bit** (Bookworm or newer) | `now-playing-pi2` |
| Raspberry Pi 3 | Raspberry Pi OS Lite **64-bit** (Bookworm or newer) | `now-playing-pi3` |

1. Flash Raspberry Pi OS Lite with Raspberry Pi Imager (set hostname, user,
   Wi-Fi and SSH there). The Pi 2 has no Wi-Fi: use Ethernet or a USB Wi-Fi stick.
2. Download `now-playing-<version>.tar.gz` from the
   [releases](https://github.com/ljbergmann/now-playing/releases), copy it to the
   Pi and run:

```bash
tar xzf now-playing-*.tar.gz && bash now-playing/pi/install.sh && sudo reboot
```

3. Scan the QR code on the screen (see [Setup](#setup)).

Status: `systemctl status now-playing`, live log: `journalctl -u now-playing -f`.

## Desktop app

| System | Download | Settings |
|---|---|---|
| macOS (Apple Silicon) | `Now-Playing-mac-<version>.zip` | native window (⌘,) |
| Windows 10/11 (64-bit) | `now-playing-windows-<version>.zip` (keep the DLL next to the exe) | native window (`S`, right-click menu) |
| Linux with a desktop (x86_64) | `now-playing-linux` | the setup page in the browser (`S`) |

Run it in a window or fullscreen on any monitor: **Fullscreen** and **Screen**
in the menu (menu bar on the Mac, right-click in the window), or double-click /
`F`; `Esc` leaves fullscreen. Keys: `1` `2` `3` look, `I` preview the clock
screen, `S` settings. The chosen look, screen and fullscreen are remembered.

The apps aren't signed by Apple/Microsoft: on the Mac right-click → Open the
first time; on Windows click "More info → Run anyway". The Windows and Linux
apps haven't been tested on real machines yet.

## Settings

The settings window / setup page covers the usual ones. Everything is stored in
a settings file: `/var/lib/now-playing/settings` on the Pi,
`~/Library/Application Support/Now Playing/settings` on a Mac,
`%APPDATA%\Now Playing\settings` on Windows, `~/.config/now-playing/settings` on
Linux. Values in the environment win over the file; on the Pi, put fixed screen
options in `/etc/now-playing/env` and `sudo systemctl restart now-playing`.

| Setting | Values |
|---|---|
| `THEME` | `vinyl` (default), `poster`, `ambient` |
| `CLOCK` | `24h` (default) or `12h` |
| `STATS` | `on` (default) or `off`: your Last.fm numbers under the track |
| `VINYLVAULT_CATALOGUE` | `on` (default) or `off`: catalogue pressings for albums you don't own |
| `PROMO` | `on` (default) or `off`: the small VinylVault mark on the clock screen |
| `ROTATE` | `auto` (default), `0`, `90`, `180`, `270` (clockwise) |
| `CONNECTOR` | e.g. `HDMI-A-1`, `DSI-1` (default: first connected screen) |
| `POLL_SECS` | seconds between Last.fm checks (default 2; every second near a track's end) |
| `SETUP_PORT` | port of the setup page (default 8737) |

**Check first:** play a song in Tidal and look at your Last.fm profile. If it
shows "Scrobbling now" right away, the display follows within a few seconds.

## How it works

```
Last.fm ──every 2 s──▶ poller thread ──▶ compose thread ──▶ 60 fps loop ──▶ screen
                       (+ cover art,      (renders a full     (crossfade,     (DRM/KMS,
                        play counts)       static frame)       spinning record) 2 buffers)
```

New screens are rendered on background threads (all cores but one); the 60 fps
loop only blends finished frames and redraws what moves.

| Source file | What it does |
|---|---|
| `src/lastfm.rs` | Polls Last.fm, cover art, play counts, "Connect with Last.fm" |
| `src/setup.rs`, `src/setup.html` | The phone setup page, Last.fm and VinylVault connect |
| `src/settings.rs` | The settings file |
| `src/vinylvault.rs` | Your shelf and the catalogue; matches albums to pressings |
| `src/scene.rs` | Renders a complete frame; setup QR screen; text layout |
| `src/themes/` | The looks (`vinyl.rs`, `poster.rs`, `ambient.rs`) and vinyl finishes (`pressing.rs`) |
| `src/draw.rs` | Shared drawing: text, shapes, cover scaling, palette, clock |
| `src/frame.rs` | Per-frame work: crossfade, colour drift, spinning record, equalizer |
| `src/display/` | Pi output without a desktop (`kms.rs`, DRM/KMS), the window for Mac, Windows and desktop Linux (`desktop.rs`), native settings (`settings_mac.rs`, `settings_win.rs`, shared logic in `settings_ui.rs`) |
| `src/fonts.rs` | Inter, built in (`assets/fonts`, SIL Open Font License) |

## Building

```bash
./pi/build.sh        # Pi 2, Pi 3 and desktop Linux (Docker) → dist/
./mac/bundle.sh      # Mac app with icon → dist/Now Playing.app
./windows/build.sh   # Windows app (MinGW-w64: brew install mingw-w64) → dist/now-playing-windows/
```

Release builds carry the app's own Last.fm key and secret, read from
`~/.config/now-playing/build.env` (`LASTFM_APP_KEY=…`, `LASTFM_APP_SECRET=…`,
register at https://www.last.fm/api/account/create) and never stored in the repo.
They end up inside the published binaries, so treat them as public and register
them under a project account. A login still needs each user's own "Allow".
Without them, the setup page asks people for their own API key.

### Develop on a Mac

```bash
cargo run --release --example mock_lastfm      # fake Last.fm + VinylVault on :9911
LASTFM_USER=x LASTFM_API_KEY=x LASTFM_API_URL=http://localhost:9911/2.0/ \
  VINYLVAULT_URL=http://localhost:9911 NP_SETTINGS=/tmp/np-settings \
  cargo run --release -- --size 800x480
curl -X POST localhost:9911/mode/stopped       # also: playing, noart, long
curl -X POST localhost:9911/finish/splatter    # shelf pressing: marbled, swirl, galaxy, clear, …
curl -X POST localhost:9911/revoke             # simulate "disconnected in VinylVault"
```

`--snapshot out.png [--idle] [--setup]` renders one frame to a file, `--bench`
times the per-frame work, `NP_PROFILE=1` prints rendering stage times.
