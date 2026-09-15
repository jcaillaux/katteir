#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy>=2", "scipy>=1.13"]
# ///
"""Cuts a cat out of footage shot on a plain blue-grey backdrop, for Katteir.

Dev machine only (CLAUDE.md §6); needs ffmpeg with FFV1. The cat is keyed
out by colour difference: red minus blue is below zero on the blue-grey
backdrop and far above it on a ginger cat. The matte is then pulled in by
half a pixel and feathered, so the outline stays smooth once enlarged.
Soft edges take their colour from the fur just inside them rather than from
the backdrop showing through, so there's no pale rim; the backdrop's colour
is a smooth fit of the frame's own backdrop pixels. Writes two lossless
clips with alpha (FFV1, BT.709 limited range):

- entry.mkv: from --entry-start up to --loop-start, at the source rate;
- sleep.mkv: the loop, from --loop-start, at --loop-fps. Two kinds:
  - --loop pingpong (the default) plays forward to --loop-end, then back.
    No frame is ever blended, so the fur never smears. Put both ends at the
    top or bottom of a breath, where the motion turns anyway.
  - --loop blend runs up to --loop-end, its last --fade seconds blended
    into the frames just before --loop-start: AI footage never comes back
    to the same frame, so a plain cut would jump.

tools/encode.sh then turns each into Katteir's stacked-alpha AV1.

Usage: uv run tools/cutout.py SRC OUT_DIR --entry-start S --loop-start S --loop-end S [--loop blend]
"""

import argparse
import json
import subprocess
import tempfile
from fractions import Fraction
from pathlib import Path

import numpy as np
from scipy import ndimage

# Red minus blue (0-255 scale): at or below KEY_LOW it's backdrop, at or above
# KEY_HIGH it's cat, soft in between.
KEY_LOW = 5.0
KEY_HIGH = 45.0
# Pixels below this count as backdrop when fitting its colour.
BACKDROP_MAX = 0.0
# Keyed at least this opaque, a pixel's own colour is fur (minus the backdrop
# showing through); below, it takes the colour of the fur nearby.
SOLID = 0.5
# The edge: pulled in by about half a pixel, then blurred this much (pixels).
FEATHER_SIGMA = 0.7
# The fur colour spreads outwards at these scales (pixels); further out, the
# cat's mean colour. Invisible, but smooth: it keeps 4:2:0 chroma clean at the
# edge and costs the encoder almost nothing.
FILL_SIGMAS = (2.0, 8.0)
# Keeps colours defined where both blended frames are transparent.
BLEND_EPSILON = 1e-3
# Katteir's clip limit (src/limits.rs, MAX_FRAMES_PER_CLIP).
MAX_FRAMES_PER_CLIP = 600


def probe(src):
    """Width, height, frame rate and frame count of the first video stream."""
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
         "-show_entries", "stream=width,height,r_frame_rate,nb_read_frames", "-of", "json", str(src)],
        capture_output=True, check=True, text=True).stdout
    stream = json.loads(out)["streams"][0]
    fps = Fraction(stream["r_frame_rate"])
    assert fps.denominator == 1, f"{src}: {fps} fps isn't a whole number"
    return stream["width"], stream["height"], int(fps), int(stream["nb_read_frames"])


def read_frames(src, width, height, count):
    """Yields the first `count` frames as RGB, decoded as BT.709 limited range."""
    frame_bytes = width * height * 3
    with subprocess.Popen(
            ["ffmpeg", "-v", "error", "-i", str(src), "-an", "-frames:v", str(count),
             "-vf", "scale=in_color_matrix=bt709:in_range=limited",
             "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], stdout=subprocess.PIPE) as ffmpeg:
        for _ in range(count):
            chunk = ffmpeg.stdout.read(frame_bytes)
            assert len(chunk) == frame_bytes, "short frame"
            yield np.frombuffer(chunk, np.uint8).reshape(height, width, 3)
    assert ffmpeg.returncode == 0, "ffmpeg failed to decode the source"


def open_writer(path, width, height, fps):
    """A lossless RGBA writer: FFV1, BT.709 limited range, full-range alpha."""
    return subprocess.Popen(
        ["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", f"{width}x{height}",
         "-framerate", str(fps), "-i", "-",
         "-vf", "scale=out_color_matrix=bt709:out_range=limited,format=yuva444p",
         "-c:v", "ffv1", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
         "-color_range", "tv", str(path)], stdin=subprocess.PIPE)


def close_writer(writer, path):
    writer.stdin.close()
    assert writer.wait() == 0, f"ffmpeg failed to write {path}"


class Keyer:
    """Keys frames of one size; the backdrop fit's basis is computed once."""

    def __init__(self, width, height):
        ys, xs = np.mgrid[0:height, 0:width].astype(np.float32)
        xs /= width
        ys /= height
        self.basis = np.stack([np.ones_like(xs), xs, ys, xs * ys, xs * xs, ys * ys], -1)
        self.shape = (height, width)

    def key(self, rgb):
        """Straight colour (float, 0-255, defined everywhere) and alpha (0-1)."""
        assert rgb.shape[:2] == self.shape
        img = rgb.astype(np.float32)
        red_minus_blue = img[..., 0] - img[..., 2]
        raw = np.clip((red_minus_blue - KEY_LOW) / (KEY_HIGH - KEY_LOW), 0.0, 1.0)
        plate = self.backdrop(img, red_minus_blue < BACKDROP_MAX)
        # Take the backdrop's share out of each partly transparent pixel.
        a = np.maximum(raw, 1.0 / 255.0)[..., None]
        fur = np.clip((img - (1.0 - a) * plate) / a, 0.0, 255.0)
        solid = raw >= SOLID
        color = np.where(solid[..., None], fur, fill_from(fur, solid))
        return color, refine(raw)

    def backdrop(self, img, is_backdrop):
        """A smooth (quadratic) fit of the backdrop colour over the frame."""
        sample = np.zeros_like(is_backdrop)
        sample[::4, ::4] = True
        sample &= is_backdrop
        assert sample.sum() > 1000, "too little backdrop to fit"
        plate = np.empty_like(img)
        rows = self.basis[sample]
        for channel in range(3):
            coef, *_ = np.linalg.lstsq(rows, img[..., channel][sample], rcond=None)
            plate[..., channel] = self.basis @ coef
        return plate


def refine(alpha):
    """Pulls the edge in by about half a pixel (half of a 3x3 erosion), then
    feathers it: the key's edge is harder than the footage's, and shows
    stair steps once enlarged."""
    choked = 0.5 * alpha + 0.5 * ndimage.minimum_filter(alpha, size=3)
    return np.clip(ndimage.gaussian_filter(choked, FEATHER_SIGMA), 0.0, 1.0)


def fill_from(color, solid):
    """Colours for the pixels outside `solid`: the fur nearest them, smoothly."""
    weight = solid.astype(np.float32)
    out = np.empty_like(color)
    filled = np.zeros(weight.shape, bool)
    for sigma in FILL_SIGMAS:
        total = ndimage.gaussian_filter(weight, sigma)
        take = (total > 1e-3) & ~filled
        for channel in range(3):
            spread = ndimage.gaussian_filter(color[..., channel] * weight, sigma)
            out[..., channel][take] = spread[take] / total[take]
        filled |= take
    out[~filled] = color[solid].mean(0) if solid.any() else 128.0
    return out


def blend(first, second, weight):
    """Mixes two keyed frames (premultiplied), `weight` of the second."""
    assert 0.0 <= weight <= 1.0
    (c1, a1), (c2, a2) = first, second
    k1 = ((1.0 - weight) * (a1 + BLEND_EPSILON))[..., None]
    k2 = (weight * (a2 + BLEND_EPSILON))[..., None]
    color = (k1 * c1 + k2 * c2) / (k1 + k2)
    return color, (1.0 - weight) * a1 + weight * a2


def to_rgba(color, alpha):
    rgba = np.concatenate([color, alpha[..., None] * 255.0], -1)
    return np.round(rgba).astype(np.uint8)


class BlendLoop:
    """From loop_start to loop_end, its last `fade` frames blended into the
    frames just before loop_start, so it wraps without a jump."""

    def __init__(self, plan, writer):
        self.plan, self.writer = plan, writer
        self.length = plan.loop_end - plan.loop_start
        self.fade_begin = plan.loop_end - plan.fade
        self.sources = {}
        self.written = 0

    def wants(self, index):
        p = self.plan
        return (index - p.loop_start) % p.step == 0 and p.loop_start - p.fade <= index < p.loop_end

    def take(self, index, keyed):
        p = self.plan
        if index < p.loop_start:
            self.sources[index] = tuple(x.astype(np.float16) for x in keyed)
            return
        if index >= self.fade_begin:
            # Weights run up towards 1 without reaching it: the frame after
            # the last one is loop_start itself.
            weight = (index - self.fade_begin + p.step) / (p.fade + p.step)
            source = self.sources.pop(index - self.length)
            keyed = blend(keyed, tuple(x.astype(np.float32) for x in source), weight)
        self.writer.stdin.write(to_rgba(*keyed).tobytes())
        self.written += 1

    def finish(self):
        assert not self.sources, "every fade source was used"
        return self.written


class PingPongLoop:
    """Forward from loop_start to loop_end, then back. The frames wait in a
    temporary file for the way back (a loop of 250 frames is about 0.9 GB)."""

    def __init__(self, plan, writer, out_dir, shape):
        self.plan, self.writer = plan, writer
        count = (plan.loop_end - plan.loop_start) // plan.step + 1
        self.store = tempfile.NamedTemporaryFile(dir=out_dir, suffix=".frames")
        self.frames = np.memmap(self.store.name, np.uint8, "w+", shape=(count,) + shape + (4,))
        self.count = 0

    def wants(self, index):
        p = self.plan
        return p.loop_start <= index <= p.loop_end and (index - p.loop_start) % p.step == 0

    def take(self, index, keyed):
        assert self.count < len(self.frames)
        rgba = to_rgba(*keyed)
        self.frames[self.count] = rgba
        self.count += 1
        self.writer.stdin.write(rgba.tobytes())

    def finish(self):
        assert self.count == len(self.frames), "every loop frame was read"
        # Back down without repeating either end: after the last frame comes
        # loop_start again.
        for k in range(self.count - 2, 0, -1):
            self.writer.stdin.write(self.frames[k].tobytes())
        written = 2 * self.count - 2
        del self.frames
        self.store.close()
        return written


def frame_plan(args, fps, frame_count):
    """Frame indices from the arguments' seconds, checked."""
    def at(seconds):
        return int(round(seconds * fps))
    assert fps % args.loop_fps == 0, f"--loop-fps must divide the source's {fps} fps"
    pingpong = args.loop == "pingpong"
    plan = argparse.Namespace(
        entry_start=at(args.entry_start), loop_start=at(args.loop_start), loop_end=at(args.loop_end),
        fade=at(args.fade), step=fps // args.loop_fps, pingpong=pingpong)
    span = plan.loop_end - plan.loop_start
    plan.frames_to_read = plan.loop_end + 1 if pingpong else plan.loop_end
    plan.loop_frames = 2 * (span // plan.step) if pingpong else span // plan.step
    assert 0 <= plan.entry_start < plan.loop_start < plan.loop_end, "times out of order"
    assert plan.frames_to_read <= frame_count, "times past the end of the source"
    assert span % plan.step == 0, "loop length must be whole loop frames"
    if not pingpong:
        assert plan.fade % plan.step == 0 and 0 < plan.fade <= plan.loop_start, "fade must be whole loop frames"
        assert plan.fade <= span, "fade longer than the loop"
    assert plan.loop_start - plan.entry_start <= MAX_FRAMES_PER_CLIP, "entry over Katteir's frame limit"
    assert plan.loop_frames <= MAX_FRAMES_PER_CLIP, "loop over Katteir's frame limit"
    return plan


def cut(src, out_dir, plan, size, fps, loop_fps):
    width, height = size
    keyer = Keyer(width, height)
    entry_path, sleep_path = out_dir / "entry.mkv", out_dir / "sleep.mkv"
    entry, sleep = open_writer(entry_path, width, height, fps), open_writer(sleep_path, width, height, loop_fps)
    loop = PingPongLoop(plan, sleep, out_dir, (height, width)) if plan.pingpong else BlendLoop(plan, sleep)
    entry_frames = 0
    for index, rgb in enumerate(read_frames(src, width, height, plan.frames_to_read)):
        in_entry = plan.entry_start <= index < plan.loop_start
        in_loop = loop.wants(index)
        if not (in_entry or in_loop):
            continue
        keyed = keyer.key(rgb)
        if in_entry:
            entry.stdin.write(to_rgba(*keyed).tobytes())
            entry_frames += 1
        if in_loop:
            loop.take(index, keyed)
    loop_frames = loop.finish()
    close_writer(entry, entry_path)
    close_writer(sleep, sleep_path)
    assert loop_frames == plan.loop_frames
    return entry_frames, loop_frames


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("src", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--entry-start", type=float, required=True, help="seconds")
    parser.add_argument("--loop-start", type=float, required=True, help="seconds; the entry ends here")
    parser.add_argument("--loop-end", type=float, required=True,
                        help="seconds: where a pingpong loop turns back, or a blend loop ends")
    parser.add_argument("--loop", choices=("pingpong", "blend"), default="pingpong")
    parser.add_argument("--fade", type=float, default=2.0, help="seconds blended at a blend loop's seam")
    parser.add_argument("--loop-fps", type=int, default=24)
    args = parser.parse_args()
    width, height, fps, frame_count = probe(args.src)
    plan = frame_plan(args, fps, frame_count)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    entry_frames, loop_frames = cut(args.src, args.out_dir, plan, (width, height), fps, args.loop_fps)
    print(f"entry.mkv: {entry_frames} frames at {fps} fps; sleep.mkv: {loop_frames} frames at {args.loop_fps} fps")


if __name__ == "__main__":
    main()
