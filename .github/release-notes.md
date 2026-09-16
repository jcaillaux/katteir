Katteir @VERSION@ for Linux on x86-64. Every N minutes of work, a cat walks onto your screen and falls asleep: that's your break. When the break is over the cat leaves by itself, or you hold a button to end it early.

## Install

Download `@PACKAGE@` below, then:

```sh
sudo apt install ./@PACKAGE@
```

It needs glibc @GLIBC@ or newer (`ldd --version` shows yours): Ubuntu 22.04 and later, Linux Mint 21 and later, Debian 12 and later. To remove it: `sudo apt remove katteir`. To check the download: `sha256sum -c SHA256SUMS`.

## Where it runs

Katteir is tested on Budgie 10.10 with the labwc Wayland compositor. It's built on freedesktop standards to work on any Linux desktop, Wayland or X11, but it hasn't been tested on KDE, GNOME, Sway or plain X11 yet.

- On Wayland compositors with the layer-shell protocol (labwc, KDE, Sway, Hyprland, niri), there's one cat per screen, drawn above everything.
- On GNOME and on X11, the cat is a full-screen window on one screen.
- The tray icon needs a desktop that shows StatusNotifierItem icons. GNOME needs the AppIndicator extension for that.

## Licence

The code is MIT OR Apache-2.0. The cat and the icons are CC BY-NC 4.0: the cat's clips are embedded in the binary, so this package may not be sold. The third-party licences are in `/usr/share/doc/katteir/`.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
