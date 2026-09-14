# Spike: AV1 (dav1d, software) → OpenGL texture → Slint

Throwaway. Answers one question before catnap commits to it: can we play the
cat as compressed video, decoded in software, drawn through the GPU, cheaply
enough?

## What it does

- `src/ivf.rs`: reads IVF, indexes frame spans, never copies frame data.
- `src/video.rs`: decoder thread. Entry clip once, then the loop clip. dav1d
  gets `&'static` slices of the file (loaded once), and pictures go to the UI
  thread through a 3-deep bounded channel. The channel does the pacing.
- `src/gl_video.rs`: uploads Y/U/V as R8 textures, and one shader turns
  BT.709 limited-range YUV into RGBA, taking alpha from the bottom half of the
  stacked frame. It renders into an RGBA texture that Slint shows via
  `BorrowedOpenGLTextureBuilder`, and restores every GL state it touches.
- `src/main.rs`: a `slint::Timer` at the clip rate pulls one frame per tick and
  requests a redraw. The upload and draw happen in `BeforeRendering`.

## Clip format

Stacked alpha: frame height = 2 × picture height. The top half is colour and
the bottom half's luma is alpha (limited range, 16–235). Made with ffmpeg,
decoding with `libvpx-vp9` so the source alpha is kept:

```sh
ffmpeg -c:v libvpx-vp9 -i in.webm -an -filter_complex \
  "[0:v]scale=-2:720,format=yuva420p,split[c][a];[a]alphaextract,format=yuv420p[m];[c]format=yuv420p[c2];[c2][m]vstack" \
  -c:v libsvtav1 -preset 8 -crf 38 -g 60 -f ivf out.ivf
```

## Build and run

From the repo root, `make run-spike` (or `make run-spike-break` for
fullscreen + see-through) does all of the below. It builds dav1d into `.deps/` on first
use; `make help` lists the targets and variables. By hand:

dav1d is linked statically from a local build (meson, ninja and nasm needed):

```sh
PKG_CONFIG_PATH=<dav1d-prefix>/lib/pkgconfig SYSTEM_DEPS_DAV1D_LINK=static cargo build --release
target/release/av1-video-spike ../../dev-assets/derived/entry_720p30.ivf \
  ../../dev-assets/derived/loop_720p30.ivf 25 1 [snapshot.pam]
```

Arguments: entry clip, loop clip, run seconds, dav1d threads, and an optional
PAM snapshot of the window taken at 6 s.

## Results (2026-09-14, i5-1235U laptop, Iris Xe, Budgie 10.10 Wayland on labwc 0.9, 1280×720 window at 1.25× scale)

Each run lasts 25 s and uses one dav1d thread, playing the entry clip and then
the loop. "CPU" is the whole process, in % of one core.

| Run | CPU | Peak RSS | dav1d | GL upload+draw | Frames |
|---|---|---|---|---|---|
| Baseline: same window, no video, redraw at 30 fps | 3% | 85 MiB | — | — | — |
| **720p, entry 30 fps + loop 30 fps** | **32%** | 141 MiB | 7.5 ms/frame | 1.3 ms | 745/745, 0 late, 0 dropped |
| **720p, entry 30 fps + loop 15 fps** | **24%** | 123 MiB | 8.1 ms/frame | 1.3 ms | 540, 1 late tick |
| 1080p, entry 30 fps + loop 30 fps | 55% | 172 MiB | 13.7 ms/frame | 4.1 ms | 169 of 744 dropped |

- So the video costs about 29 points of one core and about 56 MiB at 720p/30.
  Dropping the loop to 15 fps takes the total down to 24%.
- 1080p doesn't keep up on this machine: the UI thread misses frames. 720p is
  the working resolution.
- The dav1d ms/frame figures are wall time at real-time pacing. They are
  higher than a flat-out benchmark (4.4 ms at 720p) because the CPU clocks
  down between frames.
- Rendering is correct: the snapshot shows the right colours and clean alpha
  over the window background. A thin light fringe on the outline also shows
  on the original clip over magenta, so it comes from the source cut-out.

Binaries (release profile from CLAUDE.md: opt z, LTO, strip):

| Binary | Size | Notes |
|---|---|---|
| Linux, `slint_only` (same window and Slint features, no video) | **10.1 MB** | the baseline: Slint + winit + femtovg |
| Linux, spike, dav1d 8-bit only | **11.5 MB** | dav1d + our video code ≈ **1.3 MB** |
| Linux, spike, dav1d all bit depths | 12.3 MB | 8/10/12-bit: dav1d ≈ 2.1 MB |
| Windows, spike, dav1d all bit depths (cargo-zigbuild) | 9.4 MB | imports only system DLLs (opengl32, dwrite, UCRT). Not run: no Windows machine here |

Where the size goes: 7 MB of the 11.5 MB of code and data has named
symbols. dav1d is the largest single item; after it come Slint's own stack:

- text: `harfrust`, `skrifa`/`read_fonts`, `swash`, `zeno`, `parley` and ICU
  segmentation, pulled by `i-slint-core` through `parley`;
- SVG: `resvg` + `tiny-skia`, through `i-slint-common`;
- D-Bus: `zbus`/`zvariant`, through `i-slint-backend-winit`, plus
  `webbrowser`;
- windowing: `winit`, Wayland and X11 (both are compiled in on Linux), and
  `sctk-adwaita`, which draws Wayland title bars with `ab_glyph` and embeds the
  Cantarell font;
- image decoders (`zune-jpeg`, `png`, `image-webp`), through `image` and `resvg`.

The remaining ~4.3 MB is anonymous read-only data that LTO merged into one
pool (panic strings, tables, the font). None of these crates can be switched
off through Slint's public cargo features as used here. But most of that pool
is ICU line-breaking dictionaries, and three Cargo.toml edits to two Slint
crates take Slint from 10.1 MB to 5.2 MB. See `spikes/slint-size/`.

`src/bin/slint_only.rs` builds the baseline (`cargo build --release` builds
both binaries).

## Results on patched Slint (`patches/`, 2026-09-14 afternoon)

- **Size: 6.52 MB** stripped (budget 7 MB): Slint alone 5.19 MB, dav1d
  8-bit plus video code ~1.3 MB.
- **Rendering is correct:** a `glReadPixels` readback of a video frame at 6 s
  shows the cat with clean alpha over the background, and the stats line
  reads 179 shown, 0 late, 0 replaced at that point. The readback replaced
  `take_snapshot`, which returned empty buffers (see `src/readback.rs`).
- **Decoding keeps up:** 0–1 late ticks in every run.
- **Rendering cadence:** in the afternoon runs, many frames were replaced
  before they were drawn. An A/B test with the two binaries alternating back
  to back (20 s each) shows unpatched Slint doing the same, so the patches
  don't cause it:

  | Run (720p) | Patched: rendered | Unpatched: rendered |
  |---|---|---|
  | 15 fps loop, pair 1 | 297 | 46 |
  | 15 fps loop, pair 2 | 34 | 36 |
  | 30 fps loop | 399 | 116 |

  The session was active and unlocked, and the same unpatched binary rendered
  every frame in the morning. So the compositor stopped asking the window to
  redraw: the window was most likely covered by another window. How labwc,
  the compositor here, handles hidden windows hasn't been checked. The CPU figures from these runs aren't
  comparable with the morning ones either: dav1d took 4.8 ms/frame instead of
  7.5, which points to a different power or clock state.

Lessons for catnap (M1):

- **Pace decoding by rendering, not by a timer.** When the compositor
  throttles a hidden window, the timer keeps decoding frames that are never
  drawn, which is wasted CPU. Advance to the next frame only after the
  previous one was rendered, and pause decoding while nothing is drawn.
- **Make sure the cat window actually comes to the front.** On Wayland,
  clients can't ask to stay on top, and a new window may not be raised or
  focused. See "On top, frameless, hold to dismiss" below.
  The cat window needs fullscreen plus focus/activation (xdg-activation on
  Wayland). This is uncertain platform behaviour, so write the fallback first.

## See-through window (`SPIKE_SEE_THROUGH=1`)

The cat is drawn straight over the desktop, with no window background and no
frame. It needs no special code:

- Slint's winit backend creates every window with `with_transparent(true)`.
  Its femtovg GL setup prefers a framebuffer config with alpha, and falls back
  to an opaque one if the system has none.
- femtovg clears the frame with the window's background colour, so
  `background: transparent` clears to alpha 0. `no-frame: true` removes the
  decorations.
- Both are driven by one `see-through` property in `ui/spike.slint`, set
  before the window is shown.

Readback of a video frame at 6 s (Budgie 10.10 Wayland session on labwc,
patched Slint):

| Session | Size | Alpha 0 | Alpha 255 | Partial (edges) | Frames drawn |
|---|---|---|---|---|---|
| Wayland (native) | 1600×900 | 64.7% | 32.5% | 2.8% | 301 / 301 |
| X11 via XWayland (`env -u WAYLAND_DISPLAY`) | 1536×863 | 65.7% | 31.6% | 2.8% | 299 / 300 |

- **The output is premultiplied**, which is what compositors expect: in
  both frames no pixel has RGB > alpha, and no alpha-0 pixel carries colour.
  femtovg premultiplies our straight-alpha texture while drawing it.
- The readback proves the framebuffer content; it can't show what the
  compositor puts on screen. Visual check on the real desktop: pending.
- Untested: KDE, Sway, bare X11 without a compositor, always-on-top,
  click-through.
- Edge quality, from the readback composited over white, black and magenta
  with `overlay=alpha=premultiplied`:
  - No dark fringe. An earlier "dark outline" came from compositing the
    premultiplied readback as if it were straight alpha, not from the
    window.
  - A thin light fringe shows over dark backgrounds. It comes from the
    original cut-out, and is also visible in the opaque snapshot.
  - Faint specks: compression noise in the alpha half (luma up to 18, i.e.
    alpha ≈ 2/255; the original is 0 there). Barely visible over white.
    Possible fixes for M3: an alpha floor in the shader (treat alpha below a
    few /255 as 0), and slight alpha erosion in `tools/encode.sh` for the
    light fringe.

## On top, frameless, hold to dismiss

Switches: `SPIKE_ON_TOP=1` (Slint `always-on-top` + `no-frame`),
`SPIKE_FULLSCREEN=1` (`Window::set_fullscreen`). Both combine with
`SPIKE_SEE_THROUGH=1`.

The dismiss button is a Slint pill over the video (`ui/spike.slint`). Its
`TouchArea` `pointer-event` reports press and release to Rust. `src/hold.rs`
is a pure state machine: `now` is passed in, and it has 7 unit tests. A
33 ms timer runs only while the button is held; it updates the fill and quits
when the 5 s hold completes. Releasing loses the progress rather than
pausing it.

Tests on the labwc 0.9 session (X11 cases via XWayland):

| Case | Check | Result |
|---|---|---|
| X11, on-top + see-through | `_NET_WM_STATE` | `_NET_WM_STATE_ABOVE` ✔ |
| | `_MOTIF_WM_HINTS` | decorations off ✔ |
| | visual depth | 32 (ARGB) ✔ |
| | 1 s press (xdotool) | keeps running ✔ |
| | press and keep holding | exits 5.15 s after the press (polled every 0.25 s) ✔; logs `dismissed by hold: true` |
| X11, fullscreen + see-through | `_NET_WM_STATE` | `_NET_WM_STATE_FULLSCREEN` ✔; window 1536×864 = X11 screen |
| Wayland (native), fullscreen + see-through | readback size | 1920×1080 = panel (1536×864 logical × 1.25) ✔; 240/240 frames |
| Wayland, on-top | — | **not possible**: winit's Wayland `set_window_level` is an empty function, and xdg-shell has no "stay on top" request |

- The press tests used synthetic input (xdotool/XTEST) and only work for X11
  windows. On native Wayland nothing can inject input without root, so the
  Wayland hold test is manual. It was done on 2026-09-14 with `make run` in
  the normal Wayland session: the button was held, the log showed `dismiss:
  hold completed` and the app quit after 41 s of playback, with 786/786
  frames drawn, 0 late, 0 replaced.
- What catnap gets on Wayland today: fullscreen + see-through shows the cat
  over the desktop, and the desktop can't be clicked. The window stays above
  normal windows while it has focus, but the user can Alt-Tab away.
- A real "above everything" overlay on Wayland needs layer-shell (labwc, KDE
  and Sway have it; GNOME doesn't). winit doesn't support it, so this is the
  custom-backend case. labwc window rules might be able to force a window on
  top; not checked.
- On X11 (and on Windows/macOS through winit), always-on-top works.

## Build notes

- dav1d cross-compiles for Windows with meson, using `zig cc -target
  x86_64-windows-gnu` as the C compiler plus nasm. Cross file:
  `[binaries] c = zig cc wrapper, ar = ['zig','ar']`, host `windows/x86_64`.
- Slint's font stack (`fontique` → `yeslogic-fontconfig-sys`) links
  fontconfig by default, which needs `libfontconfig-dev` to build. Instead,
  `Cargo.toml` enables `i-slint-common/fontconfig-dlopen`, which turns on
  fontique's `fontconfig-dlopen`. Both fontique and the sys crate then load
  fontconfig at runtime: no dev package, and no libfontconfig link. Setting
  `RUST_FONTCONFIG_DLOPEN=1` alone doesn't work: it switches only the sys
  crate's API, so fontique no longer compiles.
- `send_data` in the dav1d crate boxes each packet (one small allocation per
  frame), and `get_picture` allocates an `Arc`. Both are small, but they are
  per-frame allocations, which catnap's rules forbid in the animation path.
