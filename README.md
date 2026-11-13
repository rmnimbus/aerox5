# aerox5

A Rust GUI + CLI for the **SteelSeries Aerox 5 Wireless**. It remaps every button, sets the LED
colours and effects, CPI presets, polling rate, and the sleep/dim timers. Every setting is written
into the mouse's own memory, so it keeps working without this app, on any computer, before login.

It talks to the mouse directly through Linux `hidraw` (no hidapi, libusb or daemon). It works over
the USB cable and through the 2.4 GHz dongle, and also covers the Destiny 2 and Diablo IV editions.

## What it can change

| Area | Settings |
|---|---|
| Buttons | All 9 buttons and wheel up/down → left/right/middle/back/forward click, CPI cycle, scroll up/down, disabled, any keyboard key (US-QWERTY codes), or a media key (play/pause, next, previous, volume, mute) |
| Lighting | Static colour per LED (top/middle/bottom of the strip), rainbow, reactive flash on click, start-up lighting |
| Sensitivity | 1–5 CPI presets (100–18 000, steps of 100), active preset, polling rate 125/250/500/1000 Hz |
| Power | Sleep after 0–20 min idle, dim LEDs after 0–1200 s idle |
| Info | Connection (cable/dongle), battery % and charging state, firmware version |
| Profiles | Named TOML profiles in `~/.config/aerox5/profiles/`, import from rivalcfg |

The mouse firmware has no macros, key combos or per-key modifiers in this protocol, and it can't
report its current settings. Like rivalcfg, the app treats the last-applied profile as the mouse's
state.

## Usage

```
aerox5                      # GUI
aerox5 info                 # connection, battery, firmware
aerox5 apply [profile]      # apply a profile and save it in the mouse (--no-save: session only)
aerox5 profiles             # list profiles (* = last applied)
aerox5 show [profile]       # print a profile
aerox5 actions              # every value a button can be mapped to
aerox5 import-rivalcfg ~/.config/rivalcfg/1038_1854.device.json --name mine
aerox5 reset                # factory settings, saved in the mouse
```

Binding `aerox5 apply <profile>` to a compositor key gives you quick profile switching.

Profile file (only buttons that differ from the default need listing):

```toml
[buttons]
button9 = "key:LeftSuper"
button7 = "media:VolumeUp"
button8 = "media:VolumeDown"

[sensitivity]
presets = [800, 1600, 3200]
active = 1
polling_rate = 1000

[lighting]
rainbow = false
zones = ["#ff0000", "#00ff00", "#0000ff"]
reactive = true
reactive_color = "#ffffff"

[power]
sleep_minutes = 5
dim_seconds = 30
```

## Install (AppImage)

Download `aerox5-<version>-x86_64.AppImage` from the
[releases](https://github.com/rmnimbus/aerox5/releases), then:

```
chmod +x aerox5-*-x86_64.AppImage
mv aerox5-*-x86_64.AppImage ~/Applications/aerox5.AppImage

# once per machine: let your user talk to the mouse without root
~/Applications/aerox5.AppImage udev-rule | sudo tee /etc/udev/rules.d/70-aerox5.rules
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw --action=change

# optional: app-launcher entry + icon (~/.local/share), pointing at the AppImage
~/Applications/aerox5.AppImage desktop-entry
```

The AppImage runs on any x86_64 distro with glibc 2.35 or newer (Debian 12, Ubuntu 22.04,
openSUSE Leap 16, Fedora 36+, any Arch-based distro). It bundles the Wayland, xkbcommon and X11
libraries the GUI loads. It uses the host's GPU driver (libEGL/libGL) and glibc, as AppImages must,
and the host's libxkbcommon when there is one (it reads keymaps from the host). CLI subcommands
work through the AppImage the same way: `aerox5.AppImage info`, `aerox5.AppImage apply gaming`, …

## Updates

The AppImage checks GitHub for a newer release when it starts (turn this off under **About**).
**Update now** downloads the new AppImage, checks it against GitHub's SHA-256 digest, and replaces
the running file in place, so launcher entries keep working. Then restart. From a terminal:
`aerox5.AppImage update` (or `update --check`).

## Build

```
packaging/build-in-container.sh   # AppImage, built in Debian 12 (podman or docker) → dist/
packaging/build-appimage.sh       # same, on the host (needs the host's glibc to run)
cargo build --release             # plain binary
```

The build script fetches pinned appimagetool 1.9.1 and type2-runtime 20251108 release binaries
(SHA-256 checked) into `~/.cache/aerox5-build/`.

## Protocol

`src/protocol.rs` documents every command. Sources:
- [rivalcfg](https://github.com/flozz/rivalcfg) `devices/aerox5_wireless_*.py`: command bytes,
  button slots, CPI table and key codes (tables in `src/tables.rs` were generated from it).
- [OpenRGB](https://github.com/CalcProgrammer1/OpenRGB) `SteelSeriesAeroxWirelessController`:
  64-byte packets, 0x40 wireless flag with readback, the 0x90 firmware query.
- [libratbag](https://github.com/libratbag/libratbag)/[Piper](https://github.com/libratbag/piper):
  the UI model (buttons / LEDs / resolutions / profiles). libratbag has no Aerox 5 support.

Verified on hardware (firmware 1.11.4, cable): detection, battery, firmware, applying and saving a
full profile.
