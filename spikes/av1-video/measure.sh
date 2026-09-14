#!/bin/sh
# Runs the spike on each clip set and reports process CPU and peak RSS.
# Opens a 1280x720 window for RUN_SECS each time. Usage: ./measure.sh [run_secs]
set -eu
cd "$(dirname "$0")"
RUN_SECS="${1:-25}"
BIN=target/release/av1-video-spike
CLIPS=../../dev-assets/derived
OUT="${OUT_DIR:-.}"

run() {
    label=$1; entry=$2; loop=$3; shift 3
    echo "== $label ($RUN_SECS s, dav1d 1 thread)"
    /usr/bin/time -f "   process: CPU %P of one core, peak RSS %M KiB, wall %e s" \
        "$BIN" "$CLIPS/$entry" "$CLIPS/$loop" "$RUN_SECS" 1 "$@" 2>&1 | sed 's/^/   /'
}

ls -la "$BIN" | awk '{print "binary: " $5 " bytes"}'
echo "== baseline: same window, no video, redraw at 30 fps ($RUN_SECS s)"
/usr/bin/time -f "   process: CPU %P of one core, peak RSS %M KiB, wall %e s" \
    "$BIN" - - "$RUN_SECS" 0 2>&1 | sed 's/^/   /'
run "720p, entry 30 fps + loop 30 fps" entry_720p30.ivf loop_720p30.ivf "$OUT/snapshot_720p.pam"
run "720p, entry 30 fps + loop 15 fps" entry_720p30.ivf loop_720p15.ivf
run "1080p, entry 30 fps + loop 30 fps" entry_1080p30.ivf loop_1080p30.ivf
