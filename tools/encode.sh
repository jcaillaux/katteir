#!/bin/sh
# Encodes a source clip with an alpha channel into catnap's clip format:
# stacked-alpha AV1 in IVF (colour on top, alpha as luma below, 8-bit 4:2:0).
# Dev machine only; needs ffmpeg with libsvtav1 (and libvpx for VP9 inputs).
#
# Usage: tools/encode.sh in.webm out.ivf [fps=30] [height=720] [crf=38]
set -eu

IN=$1
OUT=$2
FPS=${3:-30}
HEIGHT=${4:-720}
CRF=${5:-38}

# ffmpeg's built-in VP9 decoder drops the alpha channel; libvpx keeps it.
DECODER=""
if [ "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name -of csv=p=0 "$IN")" = "vp9" ]; then
    DECODER="-c:v libvpx-vp9"
fi

# shellcheck disable=SC2086 # DECODER is either empty or two words on purpose.
ffmpeg -v error -y $DECODER -i "$IN" -an \
    -filter_complex "[0:v]fps=$FPS,scale=-2:$HEIGHT,format=yuva420p,split[c][a];[a]alphaextract,format=yuv420p[m];[c]format=yuv420p[c2];[c2][m]vstack" \
    -c:v libsvtav1 -preset 8 -crf "$CRF" -g $((FPS * 2)) \
    -f ivf "$OUT"
ffprobe -v error -count_packets -show_entries stream=codec_name,width,height,r_frame_rate,nb_read_packets -of default=nw=1 "$OUT"
