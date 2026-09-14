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
  ~1.3 MB (see `spikes/slint-size/`). If a dependency adds megabytes, justify
  it in this file or drop it.
- Behaviour to match (observed from the original extension):
  - Cat sequence = one **entry** clip (the reference clip is ~11 s: the cat
    walks in, turns and lies down) followed by a looping **sleep** clip.
    Cross-fade ~700 ms between them. The slide-in from the right is a
    transform applied to the whole clip, not part of it.
  - Clips are cut-out cats **with alpha**, stored as "stacked alpha" video
    (see §5). Fullscreen mode (the baseline) composites them over a solid
    background; a transparent floating overlay is an optional later mode.
  - Poking the sleeping cat plays a short "stir" animation.
  - A dismiss control appears after a delay; dismiss requires a press-and-hold
    (5 s in the spike; pure state machine in `spikes/av1-video/src/hold.rs`).
  - Optional countdown badge (big white digits on `rgba(0,0,0,0.6)` rounded box).

## 2. Stack (decided — don't relitigate without a reason)

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust, stable, edition 2024 | |
| UI | `slint` `=1.17.1`, **patched** | `backend-winit` + `renderer-femtovg` (OpenGL ES). The cat is drawn from a GL texture, which the software renderer can't show, and a renderer is chosen once per process. **Never Skia.** `i-slint-core` and `i-slint-backend-winit` come from `patches/` via `[patch.crates-io]` (10.1 → 5.2 MB): no complex-script line breaking, no runtime SVG/PNG/JPEG decoding (so no image files in `.slint`; draw icons as `Path`s or pass raw RGBA), and a plain title bar on GNOME Wayland. Upgrading Slint means re-applying them (`patches/README.md`). |
| GL calls | `glow` | Raw GL for the video shader, only in `src/video/`. ~33 KiB. |
| Window/overlay | Slint `Window` props: fullscreen for the cat; `no-frame` + `always-on-top` only in optional overlay mode | |
| Cat animation | AV1 video (stacked alpha, IVF files), decoded in software by `dav1d` on a worker thread. The Y/U/V planes go up as GL textures, one shader turns them into RGBA, and Slint shows the result via `BorrowedOpenGLTextureBuilder`. `slint::Timer` paces frames at the clip rate. | `dav1d` crate + static libdav1d, 8-bit only: ~1.3 MB with our video code. 720p/30: ~32% of one core on an i5-1235U (Slint alone 3%). No ffmpeg at runtime. Hardware decode is a possible later optimisation, not a dependency. Validated in `spikes/av1-video/`. |
| Tray | `ksni` on Linux (pure Rust SNI/D-Bus), `tray-icon` on macOS/Windows | Do NOT enable `tray-icon`'s Linux backend (pulls GTK/libappindicator). |
| Notifications | `notify-rust` | |
| Config | `directories` + `serde` + `toml` | `$XDG_CONFIG_HOME/catnap/config.toml` etc. |
| Logging | `log` + `env_logger` | |
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
├── Cargo.toml
├── build.rs                 # slint_build::compile("ui/app.slint")
├── ui/
│   ├── app.slint            # exports SettingsWindow, CatWindow
│   ├── theme.slint          # colours, fonts, spacing tokens
│   └── components/          # small reusable .slint components
├── src/
│   ├── main.rs              # wiring only: build windows, start timer, tray
│   ├── config.rs            # Config struct, load/save, defaults, validation
│   ├── timer.rs             # work/break state machine (pure, no UI, no I/O)
│   ├── cats.rs              # Cat registry: entry/sleep/stir frame sets, fps
│   ├── platform/
│   │   ├── mod.rs           # trait Platform { tray, notify, display_mode }
│   │   ├── linux.rs
│   │   ├── macos.rs
│   │   └── windows.rs
│   ├── video/
│   │   ├── ivf.rs           # IVF index: frame spans, no copies (pure, tested)
│   │   ├── decode.rs        # dav1d worker thread, bounded frame queue
│   │   └── gl.rs            # Y/U/V upload + stacked-alpha shader → RGBA texture
│   └── overlay.rs           # show/hide CatWindow, slide-in, crossfade, dismiss logic
├── assets/
│   ├── cats/<name>/entry.ivf, sleep.ivf, stir.ivf
│   ├── cats/<name>/cat.toml   # fps, frame counts, size, credits, licence
│   └── icons/
├── tools/
│   └── encode.sh            # ffmpeg: source video → stacked-alpha AV1 IVF (dev-time only)
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
- **Explicit over clever.** No macros beyond `derive`/`thiserror`. No trait
  gymnastics. No `unsafe` outside `src/platform/` and `src/video/` (raw GL,
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
Idle ──start──▶ Working(deadline) ──elapsed──▶ Break(started) ──dismissed──▶ Working(new deadline)
   ▲                 │ pause                                          │ stop
   └──────stop───────┴───────────────────────────────────────────────┘
```
- Pure: `fn tick(&mut self, now: Instant) -> Vec<Event>` (bounded, small).
- Events: `BreakStarted`, `BreakEnded`, `NotifySoon { secs_left }`.
- Config: `work_minutes` (1..=180), `warn_before_secs` (0..=300),
  `min_break_secs` (0..=300; dismiss disabled until elapsed), `cat: String`.

### Display modes (`platform::display_mode()` decided at startup, overridable)
1. **Fullscreen** (default, all platforms): `CatWindow` fullscreen on the
   active display; content slides in from the right. Works on Wayland/GNOME.
2. **Overlay** (opt-in, `[display] mode = "overlay"`): borderless,
   always-on-top, transparent window sized to the display. Silently falls back
   to Fullscreen if the platform refuses (Wayland without layer-shell).
   Transparency works with femtovg on Wayland (Budgie 10.10 on labwc) and on
   X11 via XWayland:
   `background: transparent` + `no-frame`, premultiplied output, checked by GL
   readback in `spikes/av1-video`. Always-on-top (Slint `always-on-top`)
   works on X11 (`_NET_WM_STATE_ABOVE`, checked under XWayland) but does
   nothing on Wayland: winit's `set_window_level` is empty there, and
   xdg-shell has no such request. A real Wayland overlay needs layer-shell
   (labwc, KDE and Sway have it; GNOME doesn't), which winit lacks: that's the
   deferred custom-backend case. Borderless fullscreen works on both (checked).
   Untested: click-through, KDE, Sway, bare X11.

### Cat sets (`cats.rs`, `video/`)
- Each cat: `entry` (non-looping), `sleep` (looping), optional `stir`
  (non-looping, plays on click then returns to `sleep`). One IVF file each.
- Clip format: AV1, 8-bit 4:2:0, BT.709 limited range, **stacked alpha**. The
  frame height is 2 × the picture height: the top half is colour, and the
  bottom half's luma is alpha (limited range, 16–235). Pictures are 720p
  (frames 1280×1440), 30 fps for `entry`, 15 fps for `sleep`. 1080p drops
  frames on a 15 W laptop (see the spike README).
- `cat.toml`: `fps`, per-clip `frames`, `width`, `height`, `credits`,
  `license`. The loader asserts they match the IVF headers.
- Rust owns decoding and timing. Slint shows a single `image` property, fed
  from a borrowed GL texture (`slint::BorrowedOpenGLTextureBuilder`,
  straight alpha).

### Platform trait (`platform/mod.rs`)
```rust
pub trait Platform {
    fn install_tray(&mut self, on_event: Box<dyn Fn(TrayEvent)>) -> Result<(), PlatformError>;
    fn notify(&self, title: &str, body: &str) -> Result<(), PlatformError>;
    fn supports_overlay(&self) -> bool;
    fn raise_window_level(&self, window: &slint::Window) -> Result<(), PlatformError>; // macOS NSWindow.level; no-op elsewhere
}
```
Only these four things are allowed to differ by OS. Everything else is shared.

## 6. Build & run

```sh
cargo run                                  # dev (femtovg / OpenGL ES)
cargo test && cargo clippy --all-targets -- -D warnings
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
cargo zigbuild --release --target x86_64-pc-windows-gnu
cargo packager --release                   # per-platform bundles
```

Build dependencies: dav1d is built from source as a static library, 8-bit
only (`meson setup … --default-library=static -Dbitdepths=8`; needs meson,
ninja and nasm) and found through `PKG_CONFIG_PATH` plus
`SYSTEM_DEPS_DAV1D_LINK=static`. For Windows, cross-compile it with
`zig cc -target x86_64-windows-gnu` as meson's C compiler. On Linux, Slint's
font stack also needs `libfontconfig-dev`.

Encode a clip (dev machine only; ffmpeg with libvpx and libsvtav1). Decode
with `libvpx-vp9`, because ffmpeg's built-in VP9 decoder drops alpha:
```sh
tools/encode.sh in.webm assets/cats/<name>/entry.ivf 30   # stacked-alpha AV1, 720p, crf 38
```

## 7. Assets policy

- Every cat under `assets/cats/` must have a `cat.toml` with `license` and
  `credits`. Only ship assets we own or that are CC0/CC-BY with attribution
  recorded there. AI-generated clips we produce ourselves are fine.
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

1. **M0 — skeleton**: Cargo project, `app.slint` with `SettingsWindow`
   (work minutes, start/pause/stop), `timer.rs` + tests, config round-trip.
   Runs on Linux with the femtovg renderer.
2. **M1 — the cat**: `CatWindow` fullscreen, `src/video/` ported from
   `spikes/av1-video/`, one placeholder clip (a solid-colour clip is fine),
   slide-in, sleep loop, press-and-hold dismiss, `min_break_secs`.
3. **M2 — platform layer**: Linux tray (`ksni`) + notifications; verify on
   GNOME Wayland, KDE, Sway. Then macOS and Windows tray via `tray-icon`.
4. **M3 — polish**: cross-fade, stir on click, countdown badge, multiple cats,
   real assets, autostart option.
5. **M4 — ship**: `cargo-packager` bundles, CI matrix (Linux/macOS/Windows),
   size budget check in CI (fail if the stripped binary > 7 MB).
6. **Later / optional**: overlay mode with layer-shell on wlroots/KDE,
   per-app triggers, stats.

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
