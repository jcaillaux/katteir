# Patched Slint crates

Local copies of two Slint 1.17.1 crates, taken unmodified from crates.io, then
edited in their `Cargo.toml` only. Every edit is marked `PATCHED`. They're
wired in with `[patch.crates-io]`:

```toml
[patch.crates-io]
i-slint-core = { path = "patches/i-slint-core" }
i-slint-backend-winit = { path = "patches/i-slint-backend-winit" }
```

Why: desktop Slint (winit + femtovg) is 10.1 MB stripped as shipped; with
these edits it's 5.2 MB. Measurements, rendering check and costs are in
`spikes/slint-size/README.md`.

| Crate | Edit | Saves | Cost |
|---|---|---|---|
| `i-slint-core` | `parley` without `complex-scripts` | 3.8 MB | Simpler word breaking for Thai, Lao, Khmer, Burmese, CJK |
| `i-slint-core` | `std` without `svg` and `image-decoders` | ~1.15 MB with the next row | No SVG/PNG/JPEG loading at runtime in `.slint` |
| `i-slint-backend-winit` | `wayland` without `winit/wayland-csd-adwaita` | (in the row above) | Plain title bar on GNOME Wayland |

## Upgrading Slint

Slint is pinned to `=1.17.1` because these copies must match it exactly. To
upgrade: copy the new `i-slint-core` and `i-slint-backend-winit` from
`~/.cargo/registry/src/*/` over these folders, re-apply the three `PATCHED`
edits, rebuild, and rerun the size and video checks. If Slint ever makes
these switchable features, drop the patches.
