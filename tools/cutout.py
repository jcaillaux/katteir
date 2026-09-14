#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy>=2"]
# ///
"""Cuts a cat out of footage shot on a plain blue-grey backdrop, for catnap.

Dev machine only (CLAUDE.md §6); needs ffmpeg with FFV1. The cat is keyed
out by colour difference: red minus blue is below zero on the blue-grey
backdrop and far above it on a ginger cat. Soft edges then lose the
backdrop's tint (the backdrop is a smooth fit of the frame's own backdrop
pixels). Writes two lossless clips with alpha (FFV1, BT.709 limited range):

- entry.mkv: from --entry-start up to --loop-start, at the source rate;
- sleep.mkv: from --loop-start to --loop-end at --loop-fps. Its last --fade
  seconds are blended into the frames just before --loop-start, so the loop
  has no jump: AI footage never comes back to the same frame.

tools/encode.sh then turns each into catnap's stacked-alpha AV1.

Usage: uv run tools/cutout.py SRC OUT_DIR --entry-start S --loop-start S --loop-end S
"""

import argparse
import json
import subprocess
from fractions import Fraction
from pathlib import Path

import numpy as np

# Red minus blue (0-255 scale): at or below KEY_LOW it's backdrop, at or above
# KEY_HIGH it's cat, soft in between.
KEY_LOW = 5.0
KEY_HIGH = 45.0
# Pixels below this count as backdrop when fitting its colour.
BACKDROP_MAX = 0.0
# catnap's clip limit (src/limits.rs, MAX_FRAMES_PER_CLIP).
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
        assert width % 8 == 0 and height % 8 == 0, "bleed() works in 8x8 blocks"
        ys, xs = np.mgrid[0:height, 0:width].astype(np.float32)
        xs /= width
        ys /= height
        self.basis = np.stack([np.ones_like(xs), xs, ys, xs * ys, xs * xs, ys * ys], -1)
        self.shape = (height, width)

    def key(self, rgb):
        """Straight colour (float, 0-255) and alpha (0-1) of the cat."""
        assert rgb.shape[:2] == self.shape
        img = rgb.astype(np.float32)
        red_minus_blue = img[..., 0] - img[..., 2]
        alpha = np.clip((red_minus_blue - KEY_LOW) / (KEY_HIGH - KEY_LOW), 0.0, 1.0)
        plate = self.backdrop(img, red_minus_blue < BACKDROP_MAX)
        # Take the backdrop's share out of each partly transparent pixel.
        a = np.maximum(alpha, 1.0 / 255.0)[..., None]
        color = np.clip((img - (1.0 - a) * plate) / a, 0.0, 255.0)
        return color, alpha

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


def blend(first, second, weight):
    """Mixes two keyed frames (premultiplied), `weight` of the second."""
    assert 0.0 <= weight <= 1.0
    (c1, a1), (c2, a2) = first, second
    alpha = (1.0 - weight) * a1 + weight * a2
    premultiplied = (1.0 - weight) * c1 * a1[..., None] + weight * c2 * a2[..., None]
    color = premultiplied / np.maximum(alpha, 1e-6)[..., None]
    return np.clip(color, 0.0, 255.0), alpha


def to_rgba(color, alpha):
    """RGBA bytes. Transparent pixels get the nearby cat colour, so 4:2:0
    chroma at the cat's edge doesn't pick up black."""
    height, width = alpha.shape
    blocks = (height // 8, 8, width // 8, 8)
    weight = alpha.reshape(blocks).sum((1, 3))
    sums = (color * alpha[..., None]).reshape(blocks + (3,)).sum((1, 3))
    pad = ((2, 2), (2, 2))
    weight_blur = sum(np.roll(np.roll(np.pad(weight, pad), dy, 0), dx, 1)
                      for dy in range(-2, 3) for dx in range(-2, 3))[2:-2, 2:-2]
    sums_blur = sum(np.roll(np.roll(np.pad(sums, pad + ((0, 0),)), dy, 0), dx, 1)
                    for dy in range(-2, 3) for dx in range(-2, 3))[2:-2, 2:-2]
    mean = (color * alpha[..., None]).sum((0, 1)) / max(float(alpha.sum()), 1e-6)
    fill = np.where(weight_blur[..., None] > 1e-3, sums_blur / np.maximum(weight_blur, 1e-6)[..., None], mean)
    fill = fill.repeat(8, 0).repeat(8, 1)
    color = np.where(alpha[..., None] < 1.0 / 255.0, fill, color)
    rgba = np.concatenate([color, alpha[..., None] * 255.0], -1)
    return np.round(rgba).astype(np.uint8)


def frame_plan(args, fps, frame_count):
    """Frame indices from the arguments' seconds, checked."""
    def at(seconds):
        return int(round(seconds * fps))
    plan = argparse.Namespace(
        entry_start=at(args.entry_start), loop_start=at(args.loop_start), loop_end=at(args.loop_end),
        fade=at(args.fade), step=fps // args.loop_fps)
    assert fps % args.loop_fps == 0, f"--loop-fps must divide the source's {fps} fps"
    assert 0 <= plan.entry_start < plan.loop_start < plan.loop_end <= frame_count, "times out of order or range"
    assert (plan.loop_end - plan.loop_start) % plan.step == 0, "loop length must be whole loop frames"
    assert plan.fade % plan.step == 0 and 0 < plan.fade <= plan.loop_start, "fade must be whole loop frames"
    assert plan.fade <= plan.loop_end - plan.loop_start, "fade longer than the loop"
    assert plan.loop_start - plan.entry_start <= MAX_FRAMES_PER_CLIP, "entry over catnap's frame limit"
    assert (plan.loop_end - plan.loop_start) // plan.step <= MAX_FRAMES_PER_CLIP, "loop over catnap's frame limit"
    return plan


def cut(src, out_dir, plan, width, height, fps, loop_fps):
    keyer = Keyer(width, height)
    length = plan.loop_end - plan.loop_start
    fade_begin = plan.loop_end - plan.fade
    fade_sources = {}
    entry_path, sleep_path = out_dir / "entry.mkv", out_dir / "sleep.mkv"
    entry, sleep = open_writer(entry_path, width, height, fps), open_writer(sleep_path, width, height, loop_fps)
    counts = [0, 0]
    for index, rgb in enumerate(read_frames(src, width, height, plan.loop_end)):
        in_entry = plan.entry_start <= index < plan.loop_start
        on_loop_step = (index - plan.loop_start) % plan.step == 0
        in_loop = plan.loop_start <= index and on_loop_step
        is_fade_source = plan.loop_start - plan.fade <= index < plan.loop_start and on_loop_step
        if not (in_entry or in_loop or is_fade_source):
            continue
        keyed = keyer.key(rgb)
        if is_fade_source:
            fade_sources[index] = (keyed[0].astype(np.float16), keyed[1].astype(np.float16))
        if in_entry:
            entry.stdin.write(to_rgba(*keyed).tobytes())
            counts[0] += 1
        if in_loop:
            if index >= fade_begin:
                # Weights run up towards 1 without reaching it: the frame after
                # the last one is loop_start itself.
                weight = (index - fade_begin + plan.step) / (plan.fade + plan.step)
                source = fade_sources.pop(index - length)
                keyed = blend(keyed, (source[0].astype(np.float32), source[1].astype(np.float32)), weight)
            sleep.stdin.write(to_rgba(*keyed).tobytes())
            counts[1] += 1
    close_writer(entry, entry_path)
    close_writer(sleep, sleep_path)
    assert not fade_sources, "every fade source was used"
    return counts


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("src", type=Path)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--entry-start", type=float, required=True, help="seconds")
    parser.add_argument("--loop-start", type=float, required=True, help="seconds; the entry ends here")
    parser.add_argument("--loop-end", type=float, required=True, help="seconds")
    parser.add_argument("--fade", type=float, default=2.0, help="seconds blended at the loop seam")
    parser.add_argument("--loop-fps", type=int, default=12)
    args = parser.parse_args()
    width, height, fps, frame_count = probe(args.src)
    plan = frame_plan(args, fps, frame_count)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    entry_frames, sleep_frames = cut(args.src, args.out_dir, plan, width, height, fps, args.loop_fps)
    print(f"entry.mkv: {entry_frames} frames at {fps} fps; sleep.mkv: {sleep_frames} frames at {args.loop_fps} fps")


if __name__ == "__main__":
    main()
