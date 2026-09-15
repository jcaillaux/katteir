#!/usr/bin/env bash
# Renders the icon (assets/icons/tray.svg) to raw pixels for the
# places that take pixels instead of an icon name: the tray's IconPixmap and
# the X11 window icon (src/icon.rs embeds them). The patched Slint can't
# decode images at runtime, so this runs on the dev machine and the output
# is committed. Needs ffmpeg built with librsvg.
#
# Format: ARGB32, bytes A, R, G, B per pixel, straight alpha, rows top to
# bottom (the StatusNotifierItem IconPixmap layout).
set -euo pipefail

svg=assets/icons/tray.svg
for size in 16 22 32 48; do
    out="assets/icons/tray-$size.argb"
    ffmpeg -loglevel error -y -width "$size" -height "$size" -i "$svg" \
        -f rawvideo -pix_fmt argb "$out"
    bytes=$(stat -c %s "$out")
    if [ "$bytes" -ne $((size * size * 4)) ]; then
        echo "$out: $bytes bytes, expected $((size * size * 4))" >&2
        exit 1
    fi
    echo "$out: ${size}x${size}"
done
