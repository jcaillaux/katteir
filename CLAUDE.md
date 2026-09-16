# CLAUDE.md — Katteir

A tiny cross-platform desktop app: every N minutes of work, a cat takes over the
screen for a short break. Dismiss it, get back to work. Linux (X11 + Wayland),
macOS, Windows. One codebase, one small native binary, no runtime, no webview.

This file is the contract between Jonathan and Claude for this repo. Read it
fully before touching code. When in doubt, ask; do not guess.

The name: *Katt* ("cat" in Swedish and Norwegian) + *Eir*, the Norse goddess
of healing: a cat that looks after your health. Said "kat-air"; the *-eir*
spelling keeps that sound in French too, where *-ier* would read "kat-yé".
It replaced the working name catnap on 2026-09-15. The names are written
once, in `Cargo.toml` (§5, Names).

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
  tray and layer-shell cat window it's 6.52 MB. The bundled cat's clips
  are embedded too (2.83 MB, §5) and don't count against the budget: on
  2026-09-15 the binary was 9.39 MB with them, 6.56 MB without. If a
  dependency adds megabytes, justify it in this file or drop it.
- Behaviour to match (observed from the original extension):
  - Cat sequence = one **entry** clip (the reference clip is ~11 s: the cat
    walks in, turns and lies down) followed by a looping **sleep** clip.
    Cross-fade ~700 ms between them. The slide-in from the right is a
    transform applied to the whole clip, not part of it.
  - Clips are cut-out cats **with alpha**, stored as "stacked alpha" video
    (see §5), drawn straight over the desktop: the cat window is always a
    see-through overlay (§5).
  - Poking the sleeping cat plays a short "stir" animation. Not planned for
    now (set aside on 2026-09-14).
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
| Window/overlay | Slint `Window` props: the cat window is fullscreen, `no-frame`, `background: transparent` and `always-on-top`. On Wayland compositors with layer-shell it's a surface of our own on the overlay layer instead | Always an overlay; the opaque fullscreen mode was dropped on 2026-09-14. `always-on-top` does nothing on Wayland, hence layer-shell (§5). Our own Slint platform (`src/platform/backend.rs`) wraps the winit backend; `i-slint-backend-winit`, `i-slint-core` and `i-slint-renderer-femtovg` are direct dependencies for it, pinned `=1.17.1`. Layer-shell uses smithay-client-toolkit 0.19.2, wayland-client, glutin and raw-window-handle at the versions winit already pulls in: no new crates, +86 KB. |
| Cat animation | AV1 video (stacked alpha, IVF files), decoded in software by `dav1d` on a worker thread. The Y/U/V planes go up as GL textures, one shader turns them into RGBA, and Slint shows the result via `BorrowedOpenGLTextureBuilder`. `slint::Timer` paces frames at the clip rate. | `dav1d` crate + static libdav1d, 8-bit only: ~1.3 MB with our video code. 720p/30: ~32% of one core on an i5-1235U (Slint alone 3%). No ffmpeg at runtime. Hardware decode is a possible later optimisation, not a dependency. Validated in `spikes/av1-video/`. |
| Tray | Linux: our own StatusNotifierItem + dbusmenu on the D-Bus client below (+54 KB). macOS/Windows: `tray-icon` | Not `ksni`: it and `notify-rust` need zbus, measured on 2026-09-14 at +1.21 MB and 58 crates (the app 6.32 → 7.53 MB). Do NOT enable `tray-icon`'s Linux backends (GTK/libappindicator, or `ksni`). |
| Notifications | Linux: `org.freedesktop.Notifications` through our own blocking D-Bus client, `src/platform/linux/` (+35 KB, no dependencies). macOS/Windows: decided in M2 | Not `notify-rust` (zbus, see Tray). |
| Config | `directories` + `serde` + `toml` | `$XDG_CONFIG_HOME/katteir/config.toml` etc. (schema in §5). With logging, errors and our own code, M0 is 5.84 MB stripped against 5.19 MB for Slint alone, so ~0.65 MB. |
| Logging | `log` + `env_logger` | `env_logger` with default features off: no regex, no `jiff` timestamps, no colour. `RUST_LOG` still filters. |
| Errors | `thiserror` in lib code; `anyhow` only in `main.rs` | |
| Build/cross | Linux: the .deb is built in Ubuntu containers by a GitHub workflow (§6); `cargo-zigbuild` for Windows; macOS built and notarized on a Mac | |
| Packaging | `cargo-packager` 0.11.8: the .deb now; AppImage, DMG/.app, NSIS/MSI later | `cargo install cargo-packager --version 0.11.8 --locked` (`make deb` checks for it). It doesn't build the binary and doesn't find dependencies (§6). No rpm. Third-party notices: `cargo-about` 0.9.2 (`cargo install cargo-about --version 0.9.2 --locked --features cli`, §7). |

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
katteir/
├── CLAUDE.md
├── README.md                # for people: what Katteir is, install, build, settings, credits
├── LICENSE-MIT, LICENSE-APACHE  # the code's licence: MIT OR Apache-2.0 (§7)
├── Makefile                 # dev entry points: make run, test, clippy, deb, run-spike (make help)
├── .github/workflows/deb.yml  # the .deb, built in ubuntu:26.04 and ubuntu:22.04 (glibc 2.43 and 2.35, §6)
├── Cargo.toml               # also the app's names and the .deb: [package.metadata.packager] (§5, §6)
├── build.rs                 # compiles the UI; hands the names to Rust (env!) and Slint (@app-info), and the clip-fields feature to Slint
├── ui/
│   ├── app.slint            # exports SettingsWindow, CatWindow
│   ├── cat.slint            # CatWindow: the overlay (video, countdown badge, hold pill)
│   ├── theme.slint          # colours, fonts, spacing tokens
│   └── components/          # small reusable .slint components
├── src/
│   ├── main.rs              # wiring only: build windows, start timer, tray
│   ├── app.rs               # the app's names (NAME, ID, DIR...), from Cargo.toml via build.rs
│   ├── config.rs            # Config struct, load/save, defaults, validation
│   ├── limits.rs            # fixed limits (§4)
│   ├── icon.rs              # the icon as ARGB pixels (tray IconPixmap, X11 window icon)
│   ├── timer.rs             # work/break state machine (pure, no UI, no I/O)
│   ├── hold.rs              # press-and-hold state machine (pure, tested)
│   ├── autostart.rs         # start at login: the XDG autostart entry (a setting, off by default)
│   ├── migrate.rs           # one-time move from the working name catnap (config folder, autostart entry)
│   ├── cats.rs              # which clips play: the bundled ginger cat or the configured pair
│   ├── platform/
│   │   ├── mod.rs           # Platform: what differs by OS (notifications, tray)
│   │   ├── backend.rs       # our Slint platform: winit for all, layer-shell for the cat
│   │   ├── linux/
│   │   │   ├── layer.rs     # the cat window on the Wayland overlay layer (sctk + EGL + FemtoVG)
│   │   │   ├── wire.rs      # D-Bus wire format (pure, tested)
│   │   │   ├── bus.rs       # blocking session-bus connection: auth, Hello, calls, split
│   │   │   ├── notify.rs    # org.freedesktop.Notifications on a worker thread
│   │   │   ├── instance.rs  # one instance per session: owns the app id on D-Bus, or asks it to Show
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
│   ├── LICENSE.md           # the assets' licence, CC BY-NC 4.0; its full text beside it
│   ├── cats/ginger/         # the bundled cat (CC BY-NC 4.0, embedded with include_bytes!): AI footage, prompt.txt
│   ├── app.desktop          # desktop entry template: make install-desktop fills in names and Exec
│   └── icons/               # CC BY-NC 4.0; the .argb files are rendered by tools/icons.sh
│       ├── app.svg          # the app icon: desktop entry, docks, menus, notifications (§7)
│       ├── app-48.argb      # the same at 48 px: the X11 window icon
│       ├── tray.svg         # the tray icon, the cat's face alone ($XDG_RUNTIME_DIR/katteir/)
│       └── tray-<px>.argb   # the same at 16/22/32/48 px: the tray's IconPixmap
├── tools/
│   ├── about.toml, notices.hbs  # cargo-about config and template: third-party notices (make notices)
│   ├── cutout.py            # numpy + scipy via uv: footage on a plain backdrop → entry + loop with alpha (dev-time only)
│   ├── encode.sh            # ffmpeg: source video → stacked-alpha AV1 IVF (dev-time only)
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
TOML at `$XDG_CONFIG_HOME/katteir/config.toml` (platform equivalent elsewhere):
```toml
[timer]
work_minutes = 25         # 1..=180
warn_before_secs = 60     # 0..=300, 0 = no warning
break_secs = 300          # 10..=3600, how long the cat stays

[cat]
name = "ginger"           # bundled cat, assets/cats/<name> (there's only one so far)
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
- **The clip fields are a dev tool** (decided 2026-09-16): the settings
  window shows them only in builds with the `clip-fields` Cargo feature,
  which `make run` turns on (`RUN_FEATURES`). `make build`, `make deb` and
  the workflow leave it off, so packages don't show them. The paths set
  in the file still work there, and Save keeps them. build.rs hands the
  feature to Slint as the constant `AppInfo.clip-fields`, and the fields
  are `if`s on it, so the Rust code is the same in both builds. Slint
  still compiles the hidden fields in: the `if`s cost 15 KB in both.
- **Warn, don't refuse:** a clip that's missing or unusable is still saved
  (it may be on a drive that isn't mounted yet). The field turns red, the
  Save notice says why, and the cat window falls back to the bundled cat. The
- **The clip fields are a dev tool** (decided 2026-09-16): the settings
  window shows them only in builds with the `clip-fields` Cargo feature,
  which `make run` turns on (`RUN_FEATURES`). `make build`, `make deb` and
  the workflow leave it off, so packages don't show them. The paths set
  in the file still work there, and Save keeps them. build.rs hands the
  feature to Slint as the constant `AppInfo.clip-fields`, and the fields
  are `if`s on it, so the Rust code is the same in both builds. Slint
  still compiles the hidden fields in: the `if`s cost 15 KB in both.
  same goes for setting only one of the two clips. Only relative paths are
  dropped, because the app can't know what they're relative to.

### Cat window (`overlay.rs`, `ui/cat.slint`)
Always an overlay (decided 2026-09-14; the opaque fullscreen mode was
dropped): fullscreen, `no-frame`, `background: transparent` and
`always-on-top`, so the cat is drawn over the desktop. The clip keeps its
shape, as big as the screen allows, at the bottom right, and slides in from
the right by its own width. A countdown badge sits top right, and the
hold-to-dismiss pill sits bottom centre.
- **The slide is the walk.** The ginger cat's footage follows the cat as it
  walks (a tracking shot on a plain backdrop), so once cut out the cat walks
  on the spot, and the slide moves it. Measured on the planted paws: it
  walks 22 px per frame of 1280 (0.41 clip widths a second), then slows and
  stops at 4.3 s. So the entry starts at 1.58 s, where the rest of the walk
  covers exactly one clip width, and the slide takes 2.6 s with
  `cubic-bezier(0.60, 0.70, 0.75, 0.90)`, which follows the paws to within
  25 px on a 1920 px screen (the former 3 s ease-out was off by 219 px).
  The cat is cut off by the clip's right edge while it walks; sliding by the
  clip's width, not the screen's, keeps that edge off-screen on every
  aspect ratio. Timing per cat comes with a second cat.
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
  - Our Slint platform (`backend.rs`) wraps the winit backend and
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
  - **Fractional scaling:** where the compositor offers
    `wp_fractional_scale_v1` and `wp_viewporter` (labwc does; checked with
    `make test-live`), each surface gets the compositor's preferred scale in
    120ths. The window renders a buffer of `round(logical × scale / 120)`
    pixels, leaves the buffer scale at 1, and a viewport shows it at the
    logical size. The dev laptop's 1.25 panel (1536×864 logical) gets
    exactly 1920×1080, not a 2× buffer the compositor shrinks. Without those
    protocols, the output's integer scale is used through `set_buffer_scale`.
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
  (non-looping, would play on click then return to `sleep`; not planned for
  now). One IVF file each.
- Clip format: AV1, 8-bit 4:2:0, BT.709 limited range, **stacked alpha**. The
  frame height is 2 × the picture height: the top half is colour, and the
  bottom half's luma is alpha (limited range, 16–235). Pictures are 720p
  (frames 1280×1440). Both clips play at the footage's own rate: 24 fps
  for the ginger cat, as AI video is 24 fps and no frames are invented to
  reach 30. Its loop ran at 12 fps until 2026-09-15, and the breathing
  moved in visible steps. 1080p drops frames on a 15 W
  laptop (see the spike README).
- **The ginger cat** (`assets/cats/ginger/`, 2.8 MB): 30 s of AI footage
  (§7) made by `tools/cutout.py` into a 13 s entry (312 frames) and a
  20.5 s loop (492).
  - The cut-out is a colour key, red minus blue: the blue-grey backdrop is
    below zero and ginger fur far above it, so no matting model is needed.
    The key's edge is harder than the footage's and showed stair steps
    once enlarged 1.5×, so the matte is pulled in by half a pixel and
    feathered (σ 0.7 px).
  - Edge pixels take the colour of the fur just inside them, not the
    backdrop's tint, which had left a pale rim. The invisible area around
    the cat is filled with smooth colour (8×8 blocks before were costly
    to encode).
  - The loop plays forward from 14.58 s to 24.83 s, then back. Both ends
    are tops of a breath (the back rises and falls about 12 px every 4.5 to
    5 s), where the motion turns anyway. AI footage never comes back to the
    same frame; the first loop (until 2026-09-15) blended its last 2 s into
    its start, and during that blend the fur looked smeared. Nothing is
    blended now. `cutout.py --loop blend` still makes that kind.
  - Encoded at crf 30 with a keyframe every 10 s. Measured on the visible
    cat only, quality is steady from frame to frame at any crf. A keyframe
    every 2 s instead cost 40 % more at the same quality.
  - Known flaws: the shadowed pale fur under the chin, in the walk, is
    partly see-through. In shadow it has the backdrop's colour and
    smoothness, so no rule tells it from the shadowed backdrop between the
    legs; a matting model would. The contact shadow is lost.
  - It's the bundled cat, embedded with `include_bytes!`. It replaced M1's
    placeholder, a flat blob drawn by ffmpeg, on 2026-09-14.
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
- Each notification replaces our previous one (`replaces_id`), so
  warnings don't pile up. Checked on Budgie Notification Server 10.10.2.
- Notifications name their icon: `app_icon` is the app id, the icon the
  package (or `make install-desktop`) installs in the icon theme, and the
  `desktop-entry` hint names our desktop entry, from which servers take the
  icon and the app's name. Until 2026-09-15 they sent neither, and showed
  no icon.
- **Linux tray:**
  - A `StatusNotifierItem` registered with `org.kde.StatusNotifierWatcher`
    as `org.kde.StatusNotifierItem-<pid>-1`. It registers again whenever the
    watcher's owner changes, for example when the panel restarts.
  - The icon is `assets/icons/tray.svg`, written to
    `$XDG_RUNTIME_DIR/katteir/` as `<app id>-tray.svg` and named through `IconName` +
    `IconThemePath`, as Chromium's tray icons do (Discord's, for example).
    The ayatana watcher behind Budgie's and Ubuntu's AppIndicator applet
    reads only icon names (`IconPixmap` isn't in its binary). The icon is
    also sent as `IconPixmap` (16, 22, 32 and 48 px from `src/icon.rs`) for
    hosts that ignore icon folders. The 16 properties are Chromium's 15 plus
    `IconPixmap`.
  - Menu (`com.canonical.dbusmenu` at `/MenuBar`): a status line in whole
    minutes (so at most one update a minute), Start/Pause/Stop, Settings…,
    Quit Katteir. On ayatana a left click opens the menu; hosts that send
    `Activate` (KDE) show the settings window instead.
  - Two threads: a reader blocked on the socket, and the tray thread, which
    handles bus messages and state updates from one bounded queue. Menu
    choices reach the UI through `slint::Weak::upgrade_in_event_loop`.
  - "Settings…", and a second launch's `Show`, bring an open settings window
    to the front by hiding and showing it again. Wayland gives an app no way
    to raise its own window (winit 0.30's `focus_window` is empty there),
    and compositors put a newly shown window in front with focus. The
    window's contents are kept; the compositor may place it again.
  - The tray's presence is `Starting`, `Shown` or `Absent`. Plain GNOME
    has no tray host, so there it's `Absent`, and the app registers anyway if
    a host appears later. Only while it's `Shown` does closing the settings
    window keep the app running (`run_event_loop_until_quit`, ended by Quit);
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
- **Window icon:** the app icon at 48 px (`src/icon.rs`), through Slint's
  `icon` property. winit sets `_NET_WM_ICON` on X11 and ignores it on
  Wayland (0.30.13 has no xdg-toplevel-icon); there the desktop entry
  supplies the icon.
- **One instance per session:** at startup, `Platform::start` claims the app
  id, `io.github.jcaillaux.Katteir`, as a bus name. If it's taken, it calls
  `Show` (at `/Instance`) on the owner (which
  opens its settings window) and returns `None`, and main exits. The owner's
  connection becomes the tray's, so the tray thread answers `Show`. Without
  a session bus there's no check.
- **Dock icon:** docks find a window's icon through its app id (Wayland) or
  class (X11) and a desktop entry of that name, not through the tray. main
  sets the app id to `io.github.jcaillaux.Katteir` (`slint::set_xdg_app_id`,
  before any window is shown), and `make install-desktop` installs
  `io.github.jcaillaux.Katteir.desktop` (from `assets/app.desktop`) and the
  app icon (`assets/icons/app.svg`) under `~/.local/share` for a dev
  checkout, then rebuilds the user icon cache and desktop database.
  Without them, the dock shows a blank disk. On the dev machine the bottom
  dock is Crystal Dock 2.16, a separate program from `budgie-panel`. It
  picks up a new desktop entry at once (it watches the applications
  folders), but its Qt icon theme doesn't see an icon added after it
  started. Its log showed "Could not find icon with name: catnap" until it
  was restarted. So a dock already running at the first install needs one
  restart; packages install the icon before the app's first launch, so
  users won't hit this. labwc's window switcher looks the icon up fresh
  each time.
- **Start at login** (`src/autostart.rs`):
  - It's a setting, off by default, applied at once by a checkbox in the
    settings window.
  - It writes or removes `$XDG_CONFIG_HOME/autostart/io.github.jcaillaux.Katteir.desktop` (the
    XDG autostart spec). Full desktop sessions honor it: GNOME, KDE, XFCE,
    Cinnamon, MATE, LXQt, Budgie (the dev machine's budgie-session does).
    Bare compositors (Sway, Hyprland, niri) need their own autostart setup.
  - The file is the setting. `Hidden=true` or
    `X-GNOME-Autostart-enabled=false`, set by a desktop's startup-apps
    tool, count as off.
  - The entry runs `katteir --autostart`, pointing at the AppImage itself
    when `$APPIMAGE` is set. The timer starts at once and the settings
    window stays hidden in the tray. It opens only if no tray icon shows
    up, or none has within 10 s, so the app is never unreachable.

### Names (`app.rs`, `build.rs`, `migrate.rs`)
The app's names are written once, in `Cargo.toml`, so a rename is two lines
there plus prose.
- The crate name, `katteir`, names the binary, the config folder
  (`$XDG_CONFIG_HOME/katteir/`), the runtime folder, the layer-shell
  namespace and the log filter (`app::DIR`, `app::LOG_FILTER`).
- `[package.metadata.packager]`, cargo-packager's own keys so packages use
  them too, has `product-name = "Katteir"`, what people see (window
  titles, tray, notifications, the menu's Quit), and
  `identifier = "io.github.jcaillaux.Katteir"`, reverse-DNS as freedesktop
  and Flathub want: the Wayland app id (X11 class), the desktop entry and icon
  names, the start-at-login entry, the single-instance D-Bus name.
- `build.rs` checks both (the id must be a valid D-Bus name) and passes them
  to Rust as `APP_NAME` and `APP_ID` (`src/app.rs`, or `concat!(env!(…))`
  where a constant needs one inside it), and to Slint as a generated
  `AppInfo` global (`import { AppInfo } from "@app-info";`). The Makefile
  reads them from `Cargo.toml` to fill in the `assets/app.desktop` template.
  File names carry no name (`assets/icons/app.svg`).
- `migrate.rs` moves what the working name left behind, once, at startup:
  `~/.config/catnap/` becomes `~/.config/katteir/` if the new folder doesn't
  exist yet, and a `catnap.desktop` start-at-login entry is replaced by the
  new one (or dropped if a desktop's tool had turned it off). Remove it once
  no catnap build is left in use.

## 6. Build & run

```sh
make run                                   # build and launch Katteir, with the clip fields (make help lists all targets)
make test && make clippy                   # Katteir's tests; clippy with warnings as errors
make test-live                             # the ignored tests: real session bus + notification server
make install-desktop                       # desktop entry + icon in ~/.local/share (dock icon); make uninstall-desktop
make deb                                   # Debian package in target/release (cargo-packager, see below)
make notices                               # third-party licence notices in target/ (cargo-about, §7)
make packaging-tools                       # cargo-packager and cargo-about at the pinned versions (CI's containers)
make run-spike                             # the AV1 video spike (builds dav1d into .deps/ first)
make run-spike-break                       # same, fullscreen + see-through
cargo run                                  # dev (femtovg / OpenGL ES)
cargo test && cargo clippy --all-targets -- -D warnings
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
tools/encode.sh in.webm assets/cats/<name>/entry.ivf 30   # stacked-alpha AV1, 720p, crf 30, a keyframe every 10 s
```

Cut a cat out of footage on a plain backdrop (dev machine only; uv and
ffmpeg with FFV1). The script declares numpy and scipy inline, so uv
fetches them into its own cache: no venv to keep. `make cat-ginger` runs
this and the two encodes with the ginger cat's measured times:
```sh
uv run tools/cutout.py src.mp4 dev-assets/derived/<name> --entry-start S --loop-start S --loop-end S [--loop pingpong|blend]
```

**The .deb** (`make deb`, configured under `[package.metadata.packager]`
in `Cargo.toml`) holds `/usr/bin/katteir`, plus our desktop entry and SVG
icon under the app id's name.
- cargo-packager names its own desktop entry and icon after the binary,
  but docks match a window to the entry named after its app id. So its
  entry is off (`generate-desktop-entry = false`), and `make deb` stages
  ours in `target/deb-files`, mapped to `/` (`deb.files`).
- It doesn't find dependencies either. The binary links only libc, libm
  and libgcc_s, and loads everything else at run time (GL/EGL,
  fontconfig, Wayland, X11, xkbcommon), which dpkg can't see. So
  `make deb` writes the Depends list (`target/deb-depends`), with libc at
  the newest version the binary needs, measured by `objdump -T`.
- The licence files (code, assets, third-party notices; §7) go in
  `/usr/share/doc/katteir/`.
- 5.2 MB on 2026-09-15. A package needs the glibc it was built against or
  newer (glibc keeps the old versions of its functions next to new ones),
  so **built here it needs glibc 2.43**: not Ubuntu 24.04 (2.39) nor
  Debian 12 (2.36). Only a few functions ask for more than 2.35: `acosf`
  and `atan2f` (2.43) and Rust std's pidfd functions (2.39). dav1d uses
  nothing newer than 2.6.
- **The workflow** (`.github/workflows/deb.yml`, on pushes to main that
  change more than Markdown files, on `v*` tags, and by hand; a newer
  push cancels a run still going) builds the .deb twice, with
  `make packaging-tools` and `make deb` as here, in two containers:
  `ubuntu:26.04` (glibc 2.43) and `ubuntu:22.04` (2.35: Ubuntu 22.04+,
  Mint 21+, Debian 12+, current Fedora and Arch). Each job then installs
  its package in its container, which checks that the Depends resolve on
  that Ubuntu and bring every library the binary names, and uploads it as
  the artifact `deb-glibc<version>`. A zigbuild for glibc 2.28, deferred
  on 2026-09-15, would only add RHEL 8 and 9 and their rebuilds; dropped
  on 2026-09-16.

## 7. Assets policy

- **Licences** (decided 2026-09-15):
  - The code is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`,
    `license` in `Cargo.toml`).
  - Our own assets, the cat and the icon, are CC BY-NC 4.0
    (`assets/LICENSE.md`, official text in
    `assets/LICENSE-CC-BY-NC-4.0.txt`). They were CC0 until then, and
    copies taken before stay CC0. The clips are embedded in the binary, so
    the binary and its packages fall under the non-commercial terms too.
  - `patches/` keeps Slint's own licence. Slint is used under its
    royalty-free licence, which asks for the Made with Slint badge where
    binaries are offered (it's in the README).
  - `make deb` puts all these licence files in `/usr/share/doc/katteir/`.
- **Third-party notices** (`make notices`): `target/THIRD-PARTY-NOTICES.txt`,
  which `make deb` ships with the licence files.
  - cargo-about 0.9.2 lists the crates. Its config, `tools/about.toml`,
    ranks the accepted licences in order of preference, so a crate
    offering several is listed under the first. It keeps to Linux x86-64
    run-time dependencies and leaves our own crate out. The template is
    `tools/notices.hbs`.
  - The Makefile then appends what cargo-about can't give: Slint's
    royalty-free licence, with the Slint crates as `cargo tree` finds
    them, and dav1d's `COPYING` (C, built from source).
  - cargo-about 0.9.2 drops `LicenseRef-*` texts even when clarified. Its
    filter (`generate.rs`) skips a clarified file that names the
    LicenseRef instead of keeping it. Its warnings about that are
    silenced (`-L error`). After an upgrade, run it without `-L error`,
    and drop the Makefile's Slint part if it's fixed.
- Every cat under `assets/cats/` must have a `cat.toml` with `license` and
  `credits`. Only ship assets we own or that are CC0/CC-BY with attribution
  recorded there. AI-generated clips we produce ourselves are fine.
- The ginger cat was generated on 2026-09-14 with ByteDance Seedance 2.5 on
  easemate.ai, text to video (`assets/cats/ginger/prompt.txt`), no
  reference image. EaseMate's terms (updated 2025-06-24) leave generated
  content with the user who made it. The 30 s source stays local in
  `dev-assets/seedance/`.
- `assets/icons/` holds icons drawn for Katteir; each file says its licence.
  - **The app icon** (`app.svg`, adopted 2026-09-16, Jonathan's design): a
    Norse round shield, blue with an iron rim and rivets, painted in black
    with the Web of Wyrd (the Norns' weaving of fate and time), and the
    sleeping ginger cat's head in front, with tabby stripes, muzzle and
    whiskers. It carries the name: *Katt*, a Norse shield for *Eir* who
    looks after health, and time for a timer. The Web of Wyrd is a modern
    symbol from the Norse revival, not a Viking-age one. There's no clip
    path in it, because Qt's SVG renderer (Crystal Dock, KDE) ignores
    clipping: the rim covers the web's corners instead. Its detail reads
    from 48 px; below that it's an orange cat on a round shield.
  - **The tray icon** (`tray.svg`) stays the plain face: at 16 to 22 px
    the app icon's detail is noise.
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
4. **M3 — polish**: real assets (done: the ginger cat, from AI footage, is
   the bundled cat), one cat per screen
   ("multiple cats" means across monitors, not a cat registry; done on
   layer-shell compositors, X11/GNOME still get one window), and starting
   at login as a setting the user turns on (never on by default; done). Dropped on
   2026-09-14: the entry→loop cross-fade (not needed) and the more compact
   settings window (the current one is compact enough).
5. **M4 — ship**: `cargo-packager` bundles, CI matrix (Linux/macOS/Windows),
   size budget check in CI (fail if the stripped binary, less the embedded
   cat's clips, is over 7 MB). The .deb is done (`make deb`, 2026-09-15),
   and since 2026-09-16 a workflow builds it for glibc 2.43 and 2.35
   (§6). Then the AppImage. Packages will be hosted as GitHub Releases
   (later). A package needs the glibc it was built against or newer (apt
   enforces it through `Depends`), so the release notes must state each
   package's baseline.
6. **Later / optional**: per-app triggers, stats, stir on click (set aside
   on 2026-09-14), and a no-OpenGL
   fallback that draws the video in software (deferred on 2026-09-14).
   Windows and macOS, deferred the same day: Linux first. The Rust unwind
   tables (`.eh_frame`, about 0.6 MB) are kept for now, also that day, so
   release builds can still be profiled and debugged.

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
