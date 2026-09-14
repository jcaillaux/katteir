# CLAUDE.md — catnap (working name)

A tiny cross-platform desktop app: every N minutes of work, a cat takes over the
screen for a short break. Dismiss it, get back to work. Linux (X11 + Wayland),
macOS, Windows. One codebase, one small native binary, no runtime, no webview.

This file is the contract between Jonathan and Claude for this repo. Read it
fully before touching code. When in doubt, ask; do not guess.

---

## 1. Why this exists / what it must be

- Inspired by Cat Gatekeeper (zokuzoku). **Clean-room reimplementation.**
  - Do NOT copy code from `zokuzoku/cat-gatekeeper-source` (source-available,
    no reuse rights) nor its cat videos, icons, name or branding (all rights
    reserved in both repos, see their `ASSETS_LICENSE.md`).
  - The old `zokuzoku/cat-gatekeeper` repo's *code* is MIT — reference only,
    we don't need any of it in a Rust app. Never vendor it.
  - Concept (timer → cat → dismiss) is not protected. UX ideas are fine.
- **Footprint is a feature.** Target ≤ 7 MB stripped binary per platform
  excluding cat assets. This was raised from 5 MB on 2026-09-14: desktop Slint
  is 5.2 MB even with `patches/` applied, and dav1d plus the video code is
  ~1.3 MB (see `spikes/slint-size/`). M1 was 7.20 MB until zbus was patched
  out of Slint (`patches/README.md`), then 6.32 MB; with M2's notifications,
  tray and layer-shell cat window it's 6.52 MB. If a dependency adds
  megabytes, justify it in this file or drop it.
- Behaviour to match (observed from the original extension):
  - Cat sequence = one **entry** clip (the reference clip is ~11 s: the cat
    walks in, turns and lies down) followed by a looping **sleep** clip.
    Cross-fade ~700 ms between them. The slide-in from the right is a
    transform applied to the whole clip, not part of it.
  - Clips are cut-out cats **with alpha**, stored as "stacked alpha" video
    (see §5), drawn straight over the desktop: the cat window is always a
    see-through overlay (§5).
  - Poking the sleeping cat plays a short "stir" animation.
  - A break lasts a set time (`break_secs`) and ends by itself. A countdown
    badge (big white digits on an `rgba(0,0,0,0.6)` rounded box) shows the
    time left, and a press-and-hold button ends the break early at any time.
    Decided 2026-09-14; this replaces the original's "dismiss appears after a
    delay" (our former `min_break_secs`).

## 2. Stack (decided — don't relitigate without a reason)

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust, stable, edition 2024 | |
| UI | `slint` `=1.17.1`, **patched** | `backend-winit` + `renderer-femtovg` (OpenGL ES). The cat is drawn from a GL texture, which the software renderer can't show, and a renderer is chosen once per process. **Never Skia.** `i-slint-core` and `i-slint-backend-winit` come from `patches/` via `[patch.crates-io]` (10.1 → 5.2 MB): no complex-script line breaking, no runtime SVG/PNG/JPEG decoding (so no image files in `.slint`; draw icons as `Path`s or pass raw RGBA), a plain title bar on GNOME Wayland, and no XDG portal settings watcher (it pulls in zbus, 0.88 MB; Slint no longer follows the desktop's colour scheme, accent, font or cursor blink, and our theme is fixed anyway). Upgrading Slint means re-applying them (`patches/README.md`). `i-slint-common` is also listed directly, only to enable `fontconfig-dlopen` (fontconfig loaded at runtime, not linked). |
| GL calls | `glow` | Raw GL for the video shader, only in `src/video/`. ~33 KiB. |
| Window/overlay | Slint `Window` props: the cat window is fullscreen, `no-frame`, `background: transparent` and `always-on-top`. On Wayland compositors with layer-shell it's a surface of our own on the overlay layer instead | Always an overlay; the opaque fullscreen mode was dropped on 2026-09-14. `always-on-top` does nothing on Wayland, hence layer-shell (§5). catnap's own Slint platform (`src/platform/backend.rs`) wraps the winit backend; `i-slint-backend-winit`, `i-slint-core` and `i-slint-renderer-femtovg` are direct dependencies for it, pinned `=1.17.1`. Layer-shell uses smithay-client-toolkit 0.19.2, wayland-client, glutin and raw-window-handle at the versions winit already pulls in: no new crates, +86 KB. |
| Cat animation | AV1 video (stacked alpha, IVF files), decoded in software by `dav1d` on a worker thread. The Y/U/V planes go up as GL textures, one shader turns them into RGBA, and Slint shows the result via `BorrowedOpenGLTextureBuilder`. `slint::Timer` paces frames at the clip rate. | `dav1d` crate + static libdav1d, 8-bit only: ~1.3 MB with our video code. 720p/30: ~32% of one core on an i5-1235U (Slint alone 3%). No ffmpeg at runtime. Hardware decode is a possible later optimisation, not a dependency. Validated in `spikes/av1-video/`. |
| Tray | Linux: our own StatusNotifierItem + dbusmenu on the D-Bus client below (+54 KB). macOS/Windows: `tray-icon` | Not `ksni`: it and `notify-rust` need zbus, measured on 2026-09-14 at +1.21 MB and 58 crates (catnap 6.32 → 7.53 MB). Do NOT enable `tray-icon`'s Linux backends (GTK/libappindicator, or `ksni`). |
| Notifications | Linux: `org.freedesktop.Notifications` through our own blocking D-Bus client, `src/platform/linux/` (+35 KB, no dependencies). macOS/Windows: decided in M2 | Not `notify-rust` (zbus, see Tray). |
| Config | `directories` + `serde` + `toml` | `$XDG_CONFIG_HOME/catnap/config.toml` etc. (schema in §5). With logging, errors and our own code, M0 is 5.84 MB stripped against 5.19 MB for Slint alone, so ~0.65 MB. |
| Logging | `log` + `env_logger` | `env_logger` with default features off: no regex, no `jiff` timestamps, no colour. `RUST_LOG` still filters. |
| Errors | `thiserror` in lib code; `anyhow` only in `main.rs` | |
| Build/cross | `cargo-zigbuild` for Linux + Windows targets; macOS built and notarized on a Mac | |
| Packaging | `cargo-packager` (AppImage, .deb, DMG/.app, MSI) | |

Release profile (`Cargo.toml`):
```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

Check versions on crates.io before adding; don't trust remembered version numbers.

## 3. Repository layout

```
catnap/
├── CLAUDE.md
├── Makefile                 # dev entry points: make run, test, clippy, run-spike (make help)
├── Cargo.toml
├── build.rs                 # slint_build::compile("ui/app.slint")
├── ui/
│   ├── app.slint            # exports SettingsWindow, CatWindow
│   ├── cat.slint            # CatWindow: the overlay (video, countdown badge, hold pill)
│   ├── theme.slint          # colours, fonts, spacing tokens
│   └── components/          # small reusable .slint components
├── src/
│   ├── main.rs              # wiring only: build windows, start timer, tray
│   ├── config.rs            # Config struct, load/save, defaults, validation
│   ├── limits.rs            # fixed limits (§4)
│   ├── icon.rs              # the icon as ARGB pixels (tray IconPixmap, X11 window icon)
│   ├── timer.rs             # work/break state machine (pure, no UI, no I/O)
│   ├── hold.rs              # press-and-hold state machine (pure, tested)
│   ├── cats.rs              # which clips play: the bundled placeholder or the configured pair
│   ├── platform/
│   │   ├── mod.rs           # Platform: what differs by OS (notifications, tray)
│   │   ├── backend.rs       # catnap's Slint platform: winit for all, layer-shell for the cat
│   │   ├── linux/
│   │   │   ├── layer.rs     # the cat window on the Wayland overlay layer (sctk + EGL + FemtoVG)
│   │   │   ├── wire.rs      # D-Bus wire format (pure, tested)
│   │   │   ├── bus.rs       # blocking session-bus connection: auth, Hello, calls, split
│   │   │   ├── notify.rs    # org.freedesktop.Notifications on a worker thread
│   │   │   ├── instance.rs  # one catnap per session: owns catnap.Instance, or asks it to Show
│   │   │   ├── menu.rs      # the tray menu over com.canonical.dbusmenu (pure, tested)
│   │   │   └── tray.rs      # StatusNotifierItem: registration, calls, state updates
│   │   ├── macos.rs
│   │   └── windows.rs
│   ├── video/
│   │   ├── mod.rs           # Clip (bytes + IVF index), probe_clip for the settings window
│   │   ├── ivf.rs           # IVF index: frame spans, no copies (pure, tested)
│   │   ├── decode.rs        # dav1d worker thread, bounded frame queue
│   │   └── gl.rs            # Y/U/V upload + stacked-alpha shader → RGBA texture
│   └── overlay.rs           # show/hide CatWindow, frame pacing, slide-in, countdown, hold-to-dismiss
├── assets/
│   ├── cats/<name>/entry.ivf, sleep.ivf, stir.ivf
│   ├── cats/<name>/cat.toml   # fps, frame counts, size, credits, licence
│   ├── cats/placeholder/    # the bundled cat (CC0, embedded with include_bytes!)
│   ├── catnap.desktop       # desktop entry; make install-desktop fills in Exec
│   └── icons/catnap-tray.svg  # the icon (CC0): tray ($XDG_RUNTIME_DIR/catnap/) and desktop entry
│       └── catnap-tray-<px>.argb  # the same at 16/22/32/48 px, rendered by tools/icons.sh
├── tools/
│   ├── encode.sh            # ffmpeg: source video → stacked-alpha AV1 IVF (dev-time only)
│   ├── placeholder.sh       # ffmpeg: draws the placeholder cat, no footage (dev-time only)
│   └── icons.sh             # ffmpeg + librsvg: the icon SVG → raw ARGB pixels (dev-time only)
├── patches/                 # Cargo.toml-patched Slint crates (see patches/README.md)
├── spikes/                  # throwaway experiments, each with a README of results
├── dev-assets/              # gitignored local test material (see §7)
└── tests/
```

## 4. Engineering rules (Jonathan's house style — non-negotiable)

Follow **TigerStyle** and **NASA's Power of Ten** as adapted for Rust:

- **Assert aggressively.** Preconditions, postconditions, invariants. Use
  `assert!`/`debug_assert!` on function boundaries; at least two assertions per
  non-trivial function. Assertions document intent — they are not error handling.
- **No unbounded loops.** Every loop has a statically obvious bound. Frame
  stepping, retries, D-Bus polls: bounded.
- **Allocate up front.** Load a cat's clip files once. Create the decoder
  and GL textures when a break starts and free them when it ends. No
  per-frame allocation in the animation path, and none in the timer tick.
  Known gap: the `dav1d` crate boxes each packet in `send_data` (see
  `spikes/av1-video/README.md`).
- **Small functions.** ≤ ~70 lines. One job. If it needs a comment to
  separate sections, split it.
- **Explicit over clever.** Don't write macros; beyond `derive`/`thiserror`,
  only use the ones a library requires (`slint::include_modules!`,
  smithay-client-toolkit's `delegate_*`). No trait gymnastics. No `unsafe` outside `src/platform/` and `src/video/` (raw GL,
  decoder FFI), and each `unsafe` block gets a `// SAFETY:` comment.
- **Errors are values.** `Result` everywhere in lib code. `unwrap`/`expect`
  only in `main.rs`, tests, or immediately after an assertion proving it safe.
- **All warnings are errors.** `#![deny(warnings)]` in CI, `clippy::pedantic`
  enabled, `clippy::all` is a build failure.
- **Determinism.** `timer.rs` takes `now: Instant` as a parameter; never calls
  the clock itself. Same for anything you want to test.
- **Fixed limits.** Max cats: 32. Max frames per clip: 600. Max clip fps: 30.
  Config values validated and clamped on load. Put limits in `src/limits.rs`.
- **Names describe what, units in the name.** `work_duration_secs`, not `work`.

Testing follows **TAP**: tests are small, independent, produce clear
pass/fail output, and test *behaviour* (state machine transitions, config
round-trip, frame index maths), not implementation. `cargo test` must be green
before any commit. `timer.rs` and `config.rs` must be 100 % testable with no
UI, no filesystem, no clock.

## 5. Core design

### Timer state machine (`timer.rs`)
```
Idle ──start──▶ Working ──deadline──▶ Break ──break_secs over, or dismiss──▶ Working
                 │    ▲                  │
            pause│    │start (resume)    │
                 ▼    │                  │
                Paused                   │
any state ──stop──▶ Idle ◀───────────────┘
```
- Pure: `fn tick(&mut self, now: Instant) -> Option<Event>`, at most one
  event per call and no allocation. Also `start` (which resumes from
  `Paused`), `pause`, `stop`, and `dismiss(now) -> Result<Event, TimerError>`,
  which ends a break early (an error outside a break). `time_left(now)` feeds
  the countdowns: time to the break while working or paused, time to the
  break's end during it.
- Events: `BreakStarted`, `BreakEnded` (from `tick` when the break is over,
  or from `dismiss`), `NotifySoon { secs_left }`. If time jumps past both the
  warning and the deadline (e.g. after a suspend), only the break starts.
  Start and pause are ignored during a break.

### Config (`config.rs`)
TOML at `$XDG_CONFIG_HOME/catnap/config.toml` (platform equivalent elsewhere):
```toml
[timer]
work_minutes = 25         # 1..=180
warn_before_secs = 60     # 0..=300, 0 = no warning
break_secs = 300          # 10..=3600, how long the cat stays

[cat]
name = "placeholder"      # bundled cat, assets/cats/<name>
entry_clip = "/abs/path/entry.ivf"   # optional; both clips set = they replace the bundled cat
loop_clip  = "/abs/path/loop.ivf"

[display]
dismiss_hold_secs = 5     # 1..=30, hold time to end a break early
```
- Missing keys take defaults. Out-of-range numbers, unsafe cat names and
  relative clip paths are fixed on load (and logged); unknown keys are an
  error. That includes the keys dropped in M1 (`min_break_secs`, `mode`): a
  file that still has them is ignored with a notice, and Save rewrites it.
- Parsing, sanitising and serialising are pure. Only `load_file` and
  `save_file` touch the disk; saving writes a temporary file, then renames it.
- Clip paths are runtime paths, so they may point at `dev-assets/` (§7). The
  settings window checks each clip with `video::probe_clip`.
- **Warn, don't refuse:** a clip that's missing or unusable is still saved
  (it may be on a drive that isn't mounted yet). The field turns red, the
  Save notice says why, and the cat window falls back to the bundled cat. The
  same goes for setting only one of the two clips. Only relative paths are
  dropped, because catnap can't know what they're relative to.

### Cat window (`overlay.rs`, `ui/cat.slint`)
Always an overlay (decided 2026-09-14; the opaque fullscreen mode was
dropped): fullscreen, `no-frame`, `background: transparent` and
`always-on-top`, so the cat is drawn over the desktop. The clip slides in from
the right (3 s), a countdown badge sits top right, and the hold-to-dismiss
pill sits bottom centre.
- Transparency works with femtovg on Wayland (Budgie 10.10 on labwc) and on
  X11 via XWayland: premultiplied output, checked by GL readback in
  `spikes/av1-video`. Borderless fullscreen works on both (checked).
- Always-on-top works on X11 (`_NET_WM_STATE_ABOVE`, checked under XWayland)
  but does nothing on Wayland: winit's `set_window_level` is empty there, and
  xdg-shell has no such request.
- **Layer-shell (Wayland):** when the compositor offers `zwlr_layer_shell_v1`
  (labwc, Sway, KDE, Hyprland, niri; not GNOME), each screen's cat window
  is a surface on that output's `overlay` layer instead
  (`src/platform/linux/layer.rs`). It sits above every window, fullscreen
  apps and panels included (exclusive zone -1), with no keyboard focus. How
  it works:
  - catnap's Slint platform (`backend.rs`) wraps the winit backend and
    forwards everything, except that `overlay_window(screen,
    CatWindow::new)` gets a `LayerWindow` adapter bound to that screen.
  - That adapter has its own Wayland connection (sctk), an EGL context via
    glutin, and Slint's `FemtoVGOpenGLRenderer` through `OpenGLInterface`.
    The overlay's rendering notifier (the video) works unchanged.
  - One timer polls the shared connection every 8 ms while any cat is
    shown, and routes each event to the window whose surface it's for.
    Frames are paced by frame callbacks, with the swap interval at 0 so
    swapping never blocks.
  - The surface, the EGL context and the GL resources exist only during a
    break. A new surface starts at buffer scale 1, so each window resets its
    scale when it makes one. Otherwise the scale kept from the last break
    made the second cat render oversized, across both screens.
  - The client has to set the cursor each time the pointer enters one of its
    surfaces (Wayland leaves it to the client), or the cursor is invisible
    over the cat and the dismiss button can't be aimed at. That's a
    `ThemedPointer`: the cursor-shape protocol where the compositor has it,
    else the cursor theme.
  - Integer output scale only; fractional scaling is untested.
  - Without layer-shell (GNOME, X11) the cat window is the winit
    fullscreen window, as before. The choice follows the compositor's
    globals, never its name. `make test-live` checks the layer-shell setup
    and counts the screens.
- **One cat per screen:** `Screens::count()` decides how many `CatWindow`s
  a break shows. It does a Wayland round trip at each break, so hot-plugged
  screens count, and it's capped at `MAX_SCREENS`. The windows are created
  on demand and reused.
  - One decoder feeds them all. Each frame (a reference-counted
    `dav1d::Picture`) goes into every window's slot, and each window
    uploads it to its own GL context. The first screen paces playback.
  - The countdown and hold progress show on every screen, and holding on
    any of them ends the break everywhere.
  - X11 and GNOME get one fullscreen window for now: a window per monitor
    there needs winit's monitor handling.
- Frames are paced by drawing: a frame is taken only once the previous one
  was drawn. The decoder thread and GL textures exist only while the window
  is shown.
- Untested: click-through, KDE, Sway, bare X11, macOS, Windows.

### Cat sets (`cats.rs`, `video/`)
- Each cat: `entry` (non-looping), `sleep` (looping), optional `stir`
  (non-looping, plays on click then returns to `sleep`). One IVF file each.
- Clip format: AV1, 8-bit 4:2:0, BT.709 limited range, **stacked alpha**. The
  frame height is 2 × the picture height: the top half is colour, and the
  bottom half's luma is alpha (limited range, 16–235). Pictures are 720p
  (frames 1280×1440), 30 fps for `entry`, 15 fps for `sleep`. 1080p drops
  frames on a 15 W laptop (see the spike README).
  The bundled placeholder is smaller: 640×360 pictures at 15 fps.
- `cat.toml`: `fps`, per-clip `frames`, `width`, `height`, `credits`,
  `license`. For now it only records credits and licence: `cats.rs` checks
  the two IVF headers against each other (same size, one rate a multiple of
  the other). Checking them against `cat.toml` comes with multiple cats (M3).
- Rust owns decoding and timing. Slint shows a single `image` property, fed
  from a borrowed GL texture (`slint::BorrowedOpenGLTextureBuilder`,
  straight alpha).

### Platform (`platform/mod.rs`)
One `Platform` struct whose fields and method bodies are picked per OS with
`#[cfg]`. It isn't a trait, because a build only ever has one
implementation. Done: `start(on_tray_action)`, `notify(summary, body)`
(never blocks; failures are only logged), `tray_available()` and
`set_tray_state()`. Still to come in M2: raising the cat window's level on
macOS (`NSWindow.level`, a no-op elsewhere).
`supports_overlay` is gone with the overlay-only cat window. Only these
things may differ by OS; everything else is shared.
- **Linux D-Bus client:**
  - Blocking, one connection per worker thread, over the session bus's
    Unix socket: `DBUS_SESSION_BUS_ADDRESS`, else `$XDG_RUNTIME_DIR/bus`.
  - `EXTERNAL` authentication with the uid of `/proc/self`.
  - Little-endian messages only. Sizes, nesting and loops are all bounded
    by `limits.rs`.
  - `make test-live` checks it against the real session bus and
    notification server without showing anything.
- Each notification replaces catnap's previous one (`replaces_id`), so
  warnings don't pile up. Checked on Budgie Notification Server 10.10.2.
- **Linux tray:**
  - A `StatusNotifierItem` registered with `org.kde.StatusNotifierWatcher`
    as `org.kde.StatusNotifierItem-<pid>-1`. It registers again whenever the
    watcher's owner changes, for example when the panel restarts.
  - The icon is `assets/icons/catnap-tray.svg`, written to
    `$XDG_RUNTIME_DIR/catnap/` and named through `IconName` +
    `IconThemePath`, as Chromium's tray icons do (Discord's, for example).
    The ayatana watcher behind Budgie's and Ubuntu's AppIndicator applet
    reads only icon names (`IconPixmap` isn't in its binary). The icon is
    also sent as `IconPixmap` (16, 22, 32 and 48 px from `src/icon.rs`) for
    hosts that ignore icon folders. The 16 properties are Chromium's 15 plus
    `IconPixmap`.
  - Menu (`com.canonical.dbusmenu` at `/MenuBar`): a status line in whole
    minutes (so at most one update a minute), Start/Pause/Stop, Settings…,
    Quit catnap. On ayatana a left click opens the menu; hosts that send
    `Activate` (KDE) show the settings window instead.
  - Two threads: a reader blocked on the socket, and the tray thread, which
    handles bus messages and state updates from one bounded queue. Menu
    choices reach the UI through `slint::Weak::upgrade_in_event_loop`.
  - The tray's presence is `Starting`, `Shown` or `Absent`. Plain GNOME
    has no tray host, so there it's `Absent`, and catnap registers anyway if
    a host appears later. Only while it's `Shown` does closing the settings
    window keep catnap running (`run_event_loop_until_quit`, ended by Quit);
    otherwise closing quits. The window says which applies.
- **Linux: standards, not desktops.** Everything goes through freedesktop
  standards that are the same on X11 and Wayland: D-Bus (notifications,
  StatusNotifierItem + dbusmenu, the single-instance name) and files
  (desktop entry, icon theme, XDG autostart). Code never branches on
  `XDG_CURRENT_DESKTOP` or the compositor. The display server only matters
  for the cat window, which has one baseline everywhere (fullscreen,
  see-through) plus extras where they work (always-on-top on X11,
  layer-shell on Wayland compositors that offer it). No XEmbed tray: it's X11-only and
  obsolete, and every current X11 desktop hosts StatusNotifierItem.
- **Window icon:** the same pixels through Slint's `icon` property. winit
  sets `_NET_WM_ICON` on X11 and ignores it on Wayland (0.30.13 has no
  xdg-toplevel-icon); there `catnap.desktop` supplies the icon.
- **One catnap per session:** at startup, `Platform::start` claims the bus
  name `catnap.Instance`. If it's taken, it calls `Show` on the owner (which
  opens its settings window) and returns `None`, and main exits. The owner's
  connection becomes the tray's, so the tray thread answers `Show`. Without
  a session bus there's no check.
- **Dock icon:** docks find a window's icon through its app id (Wayland) or
  class (X11) and a desktop entry of that name, not through the tray. main
  sets the app id to `catnap` (`slint::set_xdg_app_id`, before any window is
  shown), and `make install-desktop` installs `catnap.desktop` and the icon
  under `~/.local/share` for a dev checkout, then rebuilds the user icon
  cache and desktop database. Without them, the dock shows a blank disk. On
  the dev machine the bottom dock is Crystal Dock 2.16, a separate program
  from `budgie-panel`. It picks up a new desktop entry at once (it watches
  the applications folders), but its Qt icon theme doesn't see an icon
  added after it started. Its log showed "Could not find icon with name:
  catnap" until it was restarted. So a dock already running at the first
  install needs one restart; packages install the icon before the app's
  first launch, so users won't hit this. labwc's window switcher looks the
  icon up fresh each time.

## 6. Build & run

```sh
make run                                   # build and launch catnap (make help lists all targets)
make test && make clippy                   # catnap tests; clippy with warnings as errors
make test-live                             # the ignored tests: real session bus + notification server
make install-desktop                       # desktop entry + icon in ~/.local/share (dock icon); make uninstall-desktop
make run-spike                             # the AV1 video spike (builds dav1d into .deps/ first)
make run-spike-break                       # same, fullscreen + see-through
cargo run                                  # dev (femtovg / OpenGL ES)
cargo test && cargo clippy --all-targets -- -D warnings
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
cargo zigbuild --release --target x86_64-pc-windows-gnu
cargo packager --release                   # per-platform bundles
```

Build dependencies: no sudo, no system packages beyond cargo, git, python3
(venv), a C compiler, curl and pkg-config. `make` builds dav1d from source into
`.deps/` as a static library, 8-bit only (`meson setup …
--default-library=static -Dbitdepths=8`), and fetches the tools for that into
`.deps/` too: meson and ninja from PyPI in a virtualenv (provisional, see
below), nasm from a checksummed tarball. Cargo finds dav1d through `PKG_CONFIG_PATH` plus
`SYSTEM_DEPS_DAV1D_LINK=static`, both set by the Makefile. fontconfig is
loaded at runtime (`i-slint-common/fontconfig-dlopen`), so it needs no dev
package and isn't linked. For Windows, cross-compile dav1d with
`zig cc -target x86_64-windows-gnu` as meson's C compiler.

**Deferred: the Python setup for building dav1d.** The Makefile currently
creates the meson/ninja virtualenv with `python3 -m venv` + `pip`. The plan is
to set that venv up with `uv` instead. Until that's done, treat the Makefile's
Python setup as provisional and don't build more on it.

Encode a clip (dev machine only; ffmpeg with libvpx and libsvtav1). Decode
with `libvpx-vp9`, because ffmpeg's built-in VP9 decoder drops alpha:
```sh
tools/encode.sh in.webm assets/cats/<name>/entry.ivf 30   # stacked-alpha AV1, 720p, crf 38
```

## 7. Assets policy

- Every cat under `assets/cats/` must have a `cat.toml` with `license` and
  `credits`. Only ship assets we own or that are CC0/CC-BY with attribution
  recorded there. AI-generated clips we produce ourselves are fine.
- `assets/icons/` holds icons drawn for catnap, CC0; each file says so.
- Nothing from zokuzoku's repos is ever committed, embedded or shipped. No
  "neko", "gatekeeper", or their icon style in names or visuals.
- **One exception, local testing only:** the two original clips
  (`neko1.webm` = entry, `neko2.webm` = idle loop, from
  `https://github.com/zokuzoku/cat-gatekeeper/tree/main/assets`) may sit in
  `dev-assets/reference/`, which is gitignored. Anything derived from them
  (frames, re-encodes) stays under `dev-assets/` too. Code may only load them
  from a path given at runtime, never via `@image-url`, `include_bytes!` or
  `build.rs`, so no release binary can contain them. Never in CI.
- Keep total shipped assets ≤ 15 MB. A reference-quality cat (11 s entry at
  30 fps plus 8 s loop) is about 5 MB as stacked-alpha AV1 at 720p, crf 38.

## 8. Milestones

1. **M0 — skeleton** (done): Cargo project, `app.slint` with `SettingsWindow`
   (work minutes, start/pause/stop), `timer.rs` + tests, config round-trip.
   Runs on Linux with the femtovg renderer.
2. **M1 — the cat** (done): `CatWindow` overlay, `src/video/` ported from
   `spikes/av1-video/`, a bundled placeholder cat, slide-in, sleep loop,
   press-and-hold dismiss, and a timed break with a countdown badge (which
   replaced `min_break_secs`).
3. **M2 — platform layer** (in progress): Linux notifications and tray
   (done, both on our own D-Bus client, checked on Budgie/labwc). Still to
   verify on KDE, Sway, and GNOME, which shows no tray without the
   AppIndicator extension. Then macOS and Windows, with `tray-icon`.
4. **M3 — polish**: stir on click, real assets, one cat per screen
   ("multiple cats" means across monitors, not a cat registry; done on
   layer-shell compositors, X11/GNOME still get one window), and starting
   at login as a setting the user turns on (never on by default). Dropped on
   2026-09-14: the entry→loop cross-fade (not needed) and the more compact
   settings window (the current one is compact enough).
5. **M4 — ship**: `cargo-packager` bundles, CI matrix (Linux/macOS/Windows),
   size budget check in CI (fail if the stripped binary > 7 MB).
6. **Later / optional**: per-app triggers, stats, and a no-OpenGL
   fallback that draws the video in software (deferred on 2026-09-14).
   Windows and macOS, deferred the same day: Linux first.

## 9. How Claude should work in this repo

- Before writing code, state the plan in ≤ 5 bullets, then do it. Small PRs,
  one milestone item at a time.
- Read `Cargo.toml`, `ui/app.slint` and the module you're editing first.
- Run `cargo test` and `cargo clippy --all-targets -- -D warnings` after every
  change and report the output truthfully. Never claim green without running.
- Don't add a dependency without listing its transitive weight and putting it
  in the table in §2.
- When a platform behaviour is uncertain (Wayland, macOS window levels), say
  so and write the fallback first.
- No "TODO" without an issue-style note explaining what and why.
- Keep this file current: if a decision here changes, change it here in the
  same commit.
