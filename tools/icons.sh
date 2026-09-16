#!/usr/bin/env bash
# Renders the icons to raw pixels for the places that take pixels instead of
# an icon name (src/icon.rs embeds them): the tray icon
# (assets/icons/tray.svg) for the tray's IconPixmap, and the app icon
# (assets/icons/app.svg) for the X11 window icon. The patched Slint can't
# decode images at runtime, so this runs on the dev machine and the output
# is committed. Needs ffmpeg built with librsvg.
#
# Format: ARGB32, bytes A, R, G, B per pixel, straight alpha, rows top to
# bottom (the StatusNotifierItem IconPixmap layout).
set -euo pipefail

# render NAME SIZE...: assets/icons/NAME.svg to assets/icons/NAME-SIZE.argb.
render() {
    local name=$1
    shift
    for size in "$@"; do
        local out="assets/icons/$name-$size.argb"
        ffmpeg -loglevel error -y -width "$size" -height "$size" -i "assets/icons/$name.svg" \
            -f rawvideo -pix_fmt argb "$out"
        local bytes
        bytes=$(stat -c %s "$out")
        if [ "$bytes" -ne $((size * size * 4)) ]; then
            echo "$out: $bytes bytes, expected $((size * size * 4))" >&2
            exit 1
        fi
        echo "$out: ${size}x${size}"
    done
}

render tray 16 22 32 48
render app 48
