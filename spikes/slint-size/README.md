# Spike: how small can desktop Slint get?

Throwaway. On microcontrollers Slint fits in a few hundred KB, but the desktop
build in `spikes/av1-video` (winit + femtovg) is 10.1 MB stripped. This spike
works out how much of that is optional.

It uses the same Slint features and the same window (text + image) as
`spikes/av1-video`'s `slint_only` binary. Slint crates are overridden with
`[patch.crates-io]` by local copies whose `Cargo.toml` files are edited. Every
edit is marked `PATCHED`. The copies now live at the repo level, in
`patches/`, because catnap uses them too.

## Why patch instead of using features

The heavy parts aren't behind public `slint` features:

- `i-slint-core` asks for `parley` with `complex-scripts` (ICU line-breaking
  dictionaries for Thai, Lao, Khmer, Burmese and CJK).
- `i-slint-core`'s `std` feature, which `backend-winit` needs, forces `svg`
  (resvg plus a second text stack), `i-slint-common/markdown`,
  `image-decoders` and `locale-decimal-separator` (ICU).
- `i-slint-backend-winit`'s `wayland` feature forces `winit/wayland-csd-adwaita`
  (title bars drawn with ab_glyph and an embedded Cantarell font). It also
  always links `zbus`, `webbrowser` and `copypasta`.

## Variants

| Variant | Change | Size (stripped) | Runs? |
|---|---|---|---|
| baseline | none (`av1-video`'s `slint_only`) | 10.1 MB | yes |
| V1 | parley without `complex-scripts` | **6.3 MB** (xz: 2.0 MB) | yes (3 s smoke run) |
| V2 | V1 + core `std` without svg / markdown / decimal / image-decoders, backend without Adwaita title bars | — | **doesn't compile**: `i-slint-core` calls `decimal_separator_for_locale` (context.rs) and the styled-text parser (styled_text.rs) without a cfg gate |
| **V2b** | V1 + core `std` without svg / image-decoders, backend without Adwaita title bars | **5.2 MB** (xz: 1.7 MB) | yes |

`resvg` and `image` still appear in `cargo tree`, but only through
`slint-macros` (build time); they're no longer linked into the binary.

## Rendering check

The `Window::take_snapshot` output was all zeros, **for unpatched Slint too**,
so it proves nothing. Both probes now read the frame back with `glReadPixels`
in `AfterRendering`, before the swap (`src/main.rs`, and the unpatched control
`spikes/av1-video/src/bin/snapshot_probe.rs`). Both frames have the right
background (#2b3a4a), and the test line "Latin, العربية, 日本語, ไทย" renders
identically: Arabic is joined and right-to-left, and Japanese and Thai glyphs
are there. Window decorations aren't in the readback, so the title bar change
is untested.

## What each cut costs

| Cut | Saves | Cost to catnap |
|---|---|---|
| parley `complex-scripts` | 3.8 MB | Word-level line breaking for Thai, Lao, Khmer, Burmese and CJK falls back to simpler rules. Shaping (Arabic etc.) is unaffected. Matters only if catnap's UI is translated into those languages. |
| `svg` | ~1.15 MB | No `.svg` images in `.slint` at runtime. |
| `image-decoders` | (in the same 1.15 MB) | No PNG/JPEG decoding at runtime, so `@image-url` images won't load. Draw icons as Slint `Path`s or pass raw RGBA via `slint::Image::from_rgba8`. The cat video doesn't need decoders. |
| Adwaita title bars | (in the same 1.15 MB) | On GNOME Wayland (no server-side decorations), windows get winit's plain fallback frame without a title. KDE and Sway draw their own decorations. The cat window has no frame anyway. Untested visually. |

## Not removable with Cargo.toml edits

- `i-slint-common/markdown` and `locale-decimal-separator`: `i-slint-core`
  calls both without a cfg gate (V2). Removing them needs small source
  patches.
- `zbus`, `webbrowser`, `copypasta`: non-optional in `i-slint-backend-winit`.
  Removing them needs source patches or our own backend on the public
  `slint::platform::femtovg_renderer::FemtoVGRenderer`. The saving isn't
  measured; the symbol table suggests roughly 0.5 MB.

## Conclusion

With three Cargo.toml edits to two Slint crates, desktop Slint goes from
10.1 MB to 5.2 MB. catnap would be about 6.5 MB with the video (8-bit dav1d
plus our code ≈ 1.3 MB). The cost is keeping a local patch of the two crates
in sync with each Slint upgrade, unless the switches go upstream as features.
