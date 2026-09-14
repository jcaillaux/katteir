#!/bin/sh
# Generates the bundled placeholder cat: a flat ginger blob with ears and a
# tail, drawn by ffmpeg's geq filter (no source footage), encoded as
# stacked-alpha AV1 like every catnap clip. Our own work: CC0-1.0.
#
# Usage: tools/placeholder.sh [out_dir]    (default: assets/cats/placeholder)
# Needs ffmpeg with libsvtav1. Dev machine only.
set -eu

OUT="${1:-assets/cats/placeholder}"
W=640
H=360
FPS=15
COLOUR=0xf0a53a

# One silhouette, parameterised by a vertical offset D and a scale S (both
# ffmpeg expressions of T, the time in seconds): body, head, two ears, tail.
silhouette() {
    D=$1
    S=$2
    body="lte(pow((X-300)/(150*$S),2)+pow((Y-(250+$D))/(85*$S),2),1)"
    head="lte(hypot(X-440,Y-(165+$D)),62*$S)"
    ear1="lte(hypot(X-405,Y-(108+$D)),20*$S)"
    ear2="lte(hypot(X-475,Y-(108+$D)),20*$S)"
    tail="lte(pow((X-140)/(22*$S),2)+pow((Y-(215+$D))/(70*$S),2),1)"
    echo "255*gt($body+$head+$ear1+$ear2+$tail,0)"
}

# $1 = output file, $2 = duration (s), $3 = alpha expression
encode() {
    ffmpeg -v error -y \
        -f lavfi -i "color=c=$COLOUR:s=${W}x${H}:r=$FPS:d=$2" \
        -filter_complex "[0:v]format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='$3',format=yuva420p,split[c][a];[a]alphaextract,format=yuv420p[m];[c]format=yuv420p[c2];[c2][m]vstack" \
        -c:v libsvtav1 -preset 8 -crf 40 -g $((FPS * 2)) -svtav1-params enable-overlays=0 \
        -f ivf "$1"
}

mkdir -p "$OUT"
# Entry: 2 s of trotting (a quick bob). The slide-in itself is done by catnap.
encode "$OUT/entry.ivf" 2 "$(silhouette '6*sin(2*PI*T*2)' 1)"
# Sleep: 4 s of slow breathing; the period divides 4 s, so it loops cleanly.
encode "$OUT/sleep.ivf" 4 "$(silhouette 0 '(1+0.02*sin(2*PI*T/4))')"
ls -la "$OUT"
