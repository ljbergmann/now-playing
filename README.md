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

## Looks

Pick one with `THEME` (switch live on the Mac with keys `1` `2` `3`):

| `THEME` | Playing | Nothing playing |
|---|---|---|
| `vinyl` (default) | A record with the cover as its label, spinning at 33⅓ rpm, tonearm in the groove | Arm lifts, record spins down, big clock with the last track |
| `poster` | Cover fills the screen, heavy type over a dark fade, accent colour from the art | Cover goes dark and grey, huge clock |
| `ambient` | Cover floating on a slowly drifting colour field made from its own colours | Thin clock in the middle, last cover as a small thumbnail |

Tracks without cover art get generated art (colours derived from the track name).

### Your own pressings (VinylVault)

If you keep your records in [VinylVault](https://vinylvault.sky-lab.de), the vinyl look
can draw the record as **the pressing you own** whenever Last.fm plays an album that's on
your shelf: marbled, splatter, swirl, galaxy, picture disc, half & half, clear, glow or
solid colour, with a "YOUR PRESSING · RED MARBLED · LTD. 500" line. Albums you don't own
(or own on plain black) stay black.

1. In the VinylVault app, switch on shelf sharing and copy the link.
2. Set `VINYLVAULT_SHELF` to that link (or just the slug at its end).

The shelf is read-only and public only while sharing is on, so no password goes on the Pi.
It refreshes every 15 minutes; matching ignores "(Deluxe Edition)", " - Single" and
featured artists.

**No shelf, or an album you don't own?** The vinyl look then draws a real pressing of
that album from the public VinylVault catalogue ("ON VINYL · MOONSTONE BLUE MARBLED"),
picked once per album so it stays the same between plays. Albums you own on plain black
stay black. Only exact album + artist matches are used; otherwise the record is black.
Turn this off with `VINYLVAULT_CATALOGUE=off`.
`CLOCK=12h` switches the clock to 12-hour time.

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
| `src/scene.rs` | Renders a complete frame in the chosen look; text layout |
| `src/themes/` | The three looks: `vinyl.rs`, `poster.rs`, `ambient.rs` |
| `src/draw.rs` | Shared drawing: text, shapes, cover scaling, palette, clock |
| `src/vinylvault.rs` | Reads a shared VinylVault shelf, matches albums to your pressings |
| `src/themes/pressing.rs` | Draws the nine vinyl finishes as a spinning record |
| `src/frame.rs` | Per-frame work: crossfade, colour drift, spinning label, equalizer |
| `src/display/kms.rs` | Pi output: double-buffered DRM/KMS with vsync page flips |
| `src/display/window.rs` | Mac output: a window, for development |
| `src/fonts.rs` | Finds Inter (or another sans) via fontconfig |

## Builds

Release builds carry the app's own Last.fm API key, so people only enter their
user name. It's read from `~/.config/now-playing/build.env`
(`LASTFM_APP_KEY=…`, register at https://www.last.fm/api/account/create) and
never stored in the repo. Without it, the setup page also asks for a key. With
`LASTFM_APP_SECRET=…` as well, the page offers "Connect with Last.fm": people
tap Allow on Last.fm instead of typing their name.

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
2. Copy and install:

```bash
scp -r dist pi <user>@<pi-name>.local:now-playing/
ssh <user>@<pi-name>.local 'bash now-playing/pi/install.sh && sudo reboot'
```

3. The screen shows a QR code. Scan it with your phone: a small page served by
   the display (only reachable with the code in the QR) asks for your Last.fm
   user name, optionally your VinylVault shelf link, and the look; the display
   changes as you tap. To open it again: `sudo systemctl kill -s USR1 now-playing`.

On the Pi: `systemctl status now-playing`, live log `journalctl -u now-playing -f`.

Or download `now-playing-<version>.tar.gz` from the
[releases](https://github.com/ljbergmann/now-playing/releases), copy it to the Pi,
and run `tar xzf now-playing-*.tar.gz && bash now-playing/pi/install.sh`.

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

Set these at install time, e.g. `THEME=poster ROTATE=90 bash now-playing/pi/install.sh …`,
or edit `/etc/now-playing/env` and `sudo systemctl restart now-playing`:

| Setting | Values |
|---|---|
| `THEME` | `vinyl` (default), `poster`, `ambient` |
| `CLOCK` | `24h` (default) or `12h` |
| `VINYLVAULT_SHELF` | your shared VinylVault shelf link or slug (optional) |
| `VINYLVAULT_CATALOGUE` | `on` (default) or `off`: catalogue pressings for albums not on your shelf |
| `PROMO` | `on` (default) or `off`: the small VinylVault mark on the clock screen and "via VinylVault" under pressings |
| `STATS` | `on` (default) or `off`: one quiet line under the playing track with your Last.fm numbers, e.g. "♥ · 38th play · 1,204 plays of Ariana Grande" |
| `ROTATE` | `0`, `90`, `180`, `270` (clockwise), or `auto` (default) |
| `CONNECTOR` | e.g. `HDMI-A-1`, `DSI-1` (default: first connected screen) |

**Check first:** play a song in Tidal and look at your Last.fm profile. If it
shows "Scrobbling now" right away, the display follows track changes within a
few seconds. If the song only appears later, the display will lag behind.

## Mac app

```bash
./mac/bundle.sh      # → dist/Now Playing.app (with icon; drag it to Applications)
```

On first start the setup page opens in your browser (press S later to open it
again). Settings are saved to `~/Library/Application Support/Now Playing/settings`.
Keys 1/2/3 switch the look.
The app is ad-hoc signed, not notarized: the first time, right-click it →
Open. Without Inter installed it uses the system font (San Francisco). The
icon is drawn in code (`cargo run --release --example icon -- icon.png`).

## Develop on a Mac

```bash
cargo run --release --example mock_lastfm      # fake Last.fm on :9911
LASTFM_USER=x LASTFM_API_KEY=x LASTFM_API_URL=http://localhost:9911/2.0/ \
  cargo run --release -- --size 800x480         # window; 1/2/3 = look, I = idle, Esc = quit
curl -X POST localhost:9911/mode/stopped       # also: playing, noart, long
# fake VinylVault shelf: add VINYLVAULT_SHELF=http://localhost:9911/shelf/test, then
curl -X POST localhost:9911/finish/splatter    # marbled, swirl, galaxy, picture_disc, clear, …
```

`NP_PROFILE=1` prints how long each rendering stage takes. Other flags:
`--snapshot out.png [--idle]` renders one frame to a file, `--bench` times
the per-frame work. Environment (else the setup page's settings file): `LASTFM_USER`, `LASTFM_API_KEY`,
`POLL_SECS` (default 2; it polls every second around the expected end of a track), `FONT_REGULAR` / `FONT_BOLD` (font file overrides).
