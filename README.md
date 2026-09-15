# Katteir

Every N minutes of work, a cat walks onto your screen and falls asleep:
that's your break. When the break is over the cat leaves by itself, or you
hold a button to end the break early.

*Katt* is "cat" in Swedish and Norwegian, and *Eir* is the Norse goddess of
healing: a cat that looks after your health. Say "kat-air".

Katteir is a small native desktop app: one binary of about 9 MB, cat
included, with no runtime and no web view. It's written in Rust with Slint.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

## Features

- A work timer (25 minutes by default) that you start, pause and stop from
  the settings window or the tray menu.
- A notification before each break (60 seconds before, by default; it can
  be turned off).
- The break: a cat walks in from the right, lies down and sleeps over your
  desktop, with a countdown. It lasts 5 minutes by default; holding the
  button for 5 seconds ends it early.
- It lives in the system tray: closing the settings window keeps it running.
- It can start at login (off by default).
- You can bring your own cat (see [Your own cat](#your-own-cat)).

## Install

Linux only for now. Windows and macOS will come later.

There are no downloads yet, so build the Debian package yourself (see
[Build](#build)), then install it:

```sh
make deb
sudo apt install ./target/release/katteir_0.1.0_amd64.deb
```

The package needs the glibc version of the machine it was built on, or a
newer one: build it on the oldest system you want to install it on. To
remove it: `sudo apt remove katteir`.

You can also run Katteir straight from the build with `make run`.

## Build

You need:
- Rust 1.88 or newer ([rustup](https://rustup.rs));
- a C compiler, git, python3 with venv, make, curl, tar, sha256sum and
  pkg-config. On Debian or Ubuntu:

  ```sh
  sudo apt install build-essential git python3-venv curl pkg-config
  ```

No other system packages are needed: the first `make` builds the AV1 video
decoder, [dav1d](https://code.videolan.org/videolan/dav1d), from source into
`.deps/`, together with the tools to build it.

```sh
make run      # build and start Katteir
make test     # unit tests
make clippy   # lints, warnings as errors
make deb      # the Debian package, in target/release
make help     # everything else
```

`make deb` needs cargo-packager:
`cargo install cargo-packager --version 0.11.8 --locked`.

## Settings

Everything can be changed in the settings window. The settings are saved
in `~/.config/katteir/config.toml`:

| Setting | Default | Range |
|---|---|---|
| Work | 25 min | 1 to 180 min |
| Warn before the break | 60 s | 0 to 300 s (0 = no warning) |
| Break | 300 s | 10 to 3600 s |
| Hold to dismiss | 5 s | 1 to 30 s |

## Your own cat

A cat is two clips: an entry clip, played once, then a clip that loops
until the break ends. Both are AV1 videos with transparency, in IVF files
("stacked alpha": the colour picture on top, its transparency below). Set
their full paths in the settings window.

Two tools help make them (both need ffmpeg; details in `CLAUDE.md`):
- `tools/encode.sh` turns a video with an alpha channel into a clip;
- `tools/cutout.py` cuts a cat out of footage shot on a plain backdrop,
  then makes the entry and a seamless loop.

## Where it runs

Katteir is tested on Budgie 10.10 with the labwc Wayland compositor, on two
screens, one of them at 125 % scaling. It's built on freedesktop standards
to work on any Linux desktop, Wayland or X11, but it hasn't been tested on
KDE, GNOME, Sway or plain X11 yet.

- On Wayland compositors with the layer-shell protocol (labwc, KDE, Sway,
  Hyprland, niri), there's one cat per screen, drawn above everything,
  panels and full-screen windows included.
- On GNOME and on X11, the cat is a full-screen window on one screen.
- The tray icon needs a desktop that shows StatusNotifierItem icons, as
  most do. GNOME needs the AppIndicator extension for that. Without a tray,
  closing the settings window quits Katteir.

## Credits

- The cat: footage generated with ByteDance Seedance 2.5, then cut out and
  encoded for Katteir. It's released under CC0 (`assets/cats/ginger/`).
- The icon: drawn for Katteir, CC0.
- Inspired by Cat Gatekeeper, a browser extension by zokuzoku. Katteir is an
  independent reimplementation: it shares none of its code or assets.

## License

The cat clips and the icon are CC0. The code's licence hasn't been chosen
yet. Slint is used under its royalty-free licence, which asks for the badge
above.
