#!/usr/bin/env python3
"""Draw the evo-desktop app icon and write the macOS icon assets.

    python3 assets/icon/make_icon.py            # write assets/icon/*
    python3 assets/icon/make_icon.py --out DIR  # somewhere else

Writes, next to this file (or in `--out`):

    icon-1024.png        the master
    icon.iconset/*.png   every size macOS asks for (16 … 512, 1x and 2x)
    AppIcon.icns         the packed icon the bundle ships

Everything is drawn from the fractions in `GRID` and `MARK` below, so every
size is rendered at its own resolution and then box-filtered down (`SS`): the
16 px icon is a 16 px drawing, not a thumbnail of the 1024. Below `MARK["small_size"]`
the four lanes become three, which is the difference between four grey hairlines
and three legible strokes at 16 and 32 px.

The shape is Apple's Big Sur icon grid: a squircle 824/1024 of the canvas,
drawn as a superellipse (a plain rounded rectangle reads as a circle-cornered
square next to real macOS icons). The mark is a coordinator node feeding four
parallel lanes, like a river delta: one path per lane leaves the node
horizontally, splays over ~44% of the mark's width and arrives horizontally at
that lane's start, then runs straight to a round-capped tip — a single stroke of
one colour, so there is no seam where the curve becomes a lane. The lanes are a
little longer and shorter than each other, and their tips fade slightly, so the
mark reads as lanes at work rather than as a bulleted list.

Needs Python 3 with Pillow and NumPy, and `iconutil` (macOS) for the .icns.
"""

from __future__ import annotations

import argparse
import math
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

# --- the grid ---------------------------------------------------------------

# macOS Big Sur and later: the squircle covers 824 of the 1024 px canvas, and
# the rest is margin macOS reserves for the Dock's own shadow and selection.
GRID = 824 / 1024
# Superellipse exponent: 2 is an ellipse, a plain rounded rectangle is 0-ish;
# Apple's continuous-curvature shape sits around 5.
EXPO = 5.0
# Supersample factor. Shapes are drawn SS times larger and box-filtered down,
# which is where the anti-aliasing comes from.
SS = 4

# Gradient: indigo to deep violet, top-left to bottom-right, with a light source
# in the upper left and the top edge catching it.
TOP = (0.36, 0.41, 1.00)
BOTTOM = (0.14, 0.08, 0.40)
LIGHT_AT = (0.30, 0.18)
LIGHT_ALPHA = 0.13
HIGHLIGHT_ALPHA = 0.18
SHADOW_ALPHA = 0.16
EDGE_ALPHA = 0.20
RIM_ALPHA = 0.10

# The mark is white: one colour for the node and every lane, a soft halo around
# the node, and a soft shadow under the whole thing.
MARK_ALPHA = 1.0
GLOW_ALPHA = 0.14
MARK_SHADOW_ALPHA = 0.16

# --- the mark ---------------------------------------------------------------
# Every number is a fraction of the squircle's side, except where noted.
MARK = {
    "node_radius": 0.065,   # small: the lanes are the subject, the node the source
    "fan_span": 0.260,      # how far the curves splay before the lanes start
    "departure": 0.55,      # how far out the curve leaves the node horizontally
    "arrival": 0.55,        # …and how far back it starts flattening into its lane
    "lane_height": 0.058,   # stroke thickness
    "lane_gap": 0.054,      # between strokes
    "lanes": 4,
    # Each lane's length: a coordinator hands out work, and the lanes are not
    # all at the same point. Longest first, then trimmed to the lane count.
    "lane_lengths": (0.260, 0.235, 0.235, 0.210),
    # A lane's tip is slightly translucent; the fade suggests the lanes moving.
    "tip_fade": 0.88,
    # Below this many pixels four strokes stop resolving; three hold.
    "small_lanes": 3,
    "small_size": 48,
    # Optical centring: the mass (node plus the splayed trunk) sits left of the
    # mark's bounding box, so the box is nudged right.
    "shift": 0.008,
}


def squircle(size: int) -> np.ndarray:
    """The icon shape as a float mask, 1 inside, 0 outside."""
    # The squircle is inset from the canvas; the mask is built on it directly.
    side = size * GRID
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    cx = cy = size / 2
    # Normalised so the squircle's edge is at 1 in both axes. A square side of
    # `side` means ±side/2, which is what makes this a square superellipse and
    # not a circle.
    u = np.abs(x - cx) / (side / 2)
    v = np.abs(y - cy) / (side / 2)
    inside = (u**EXPO + v**EXPO) <= 1.0
    return inside.astype(np.float32)


def edge(size: int) -> np.ndarray:
    """The squircle shrunk by a hairline, for the inner rim."""
    side = size * GRID - size * 0.008
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    u = np.abs(x - size / 2) / (side / 2)
    v = np.abs(y - size / 2) / (side / 2)
    return ((u**EXPO + v**EXPO) <= 1.0).astype(np.float32)


def gradient(size: int) -> np.ndarray:
    """The background, top-left to bottom-right, as float RGB 0…1."""
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    t = ((x + y) / (2 * (size - 1))).clip(0, 1)
    top = np.array(TOP, np.float32)
    bottom = np.array(BOTTOM, np.float32)
    return top + (bottom - top) * t[..., None]


def canvas(size: int) -> tuple[np.ndarray, np.ndarray]:
    """The squircle's surface, supersampled: `(rgb, alpha)`."""
    s = size * SS
    rgb = gradient(s)
    alpha = squircle(s)
    y, x = np.mgrid[0:s, 0:s].astype(np.float32)
    xx, yy = x / (s - 1), y / (s - 1)

    # A light source in the upper left, and the top edge catching it: this is
    # what keeps the surface from reading as flat colour.
    light = np.clip(1 - np.hypot(xx - LIGHT_AT[0], yy - LIGHT_AT[1]) / 0.75, 0, 1) ** 2
    rgb = rgb + (light * LIGHT_ALPHA)[..., None]
    rgb = rgb + (np.clip(1.0 - yy / 0.5, 0, 1) ** 2 * HIGHLIGHT_ALPHA)[..., None]
    rgb = rgb - (np.clip((yy - 0.55) / 0.45, 0, 1) ** 2 * EDGE_ALPHA)[..., None]
    # A hairline rim just inside the edge, the way macOS icons catch the light.
    rgb = rgb + ((np.clip(alpha - edge(s), 0, 1)) * RIM_ALPHA)[..., None]
    return rgb, alpha


def bezier(p0, p1, p2, p3, steps=140) -> list[tuple[float, float]]:
    """Points along a cubic Bézier, P0…P3."""
    out = []
    for i in range(steps + 1):
        t = i / steps
        u = 1 - t
        out.append(
            (
                u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
            )
        )
    return out


def stamp_path(mask: Image.Image, path: list[tuple[float, float]], radius: float) -> None:
    """Draw a round-capped stroke by stamping circles along `path`.

    Stamping rather than `line` because it gives round joins and caps for free,
    and because a Bézier's polyline has joins a plain line would show.
    """
    draw = ImageDraw.Draw(mask)
    step = max(1.0, radius * 0.35)
    for (x0, y0), (x1, y1) in zip(path, path[1:]):
        spans = max(1, int(math.hypot(x1 - x0, y1 - y0) / step))
        for i in range(spans + 1):
            t = i / spans
            x, y = x0 + (x1 - x0) * t, y0 + (y1 - y0) * t
            draw.ellipse([x - radius, y - radius, x + radius, y + radius], fill=255)


def lanes_for(size: int) -> int:
    """How many lanes this size gets: four, or three when four stop resolving."""
    return MARK["lanes"] if size >= MARK["small_size"] else MARK["small_lanes"]


def lane_path(fan_span, node, lane_y, tip_x) -> list[tuple[float, float]]:
    """One lane's whole path: out of the node, through the splay, on to its tip.

    Horizontal at both ends — it leaves the node the way water leaves a delta
    and arrives at the lane already flat — which is what keeps the lanes
    parallel while the part near the node fans out.
    """
    (nx, ny) = node
    end = (nx + fan_span, lane_y)
    distance = math.hypot(end[0] - nx, end[1] - ny)
    depart = (nx + MARK["departure"] * distance, ny)
    arrive = (end[0] - MARK["arrival"] * fan_span, lane_y)
    return bezier((nx, ny), depart, arrive, end) + [(tip_x, lane_y)]


def mark_masks(size: int) -> tuple[Image.Image, Image.Image]:
    """`(node, lanes)` coverage masks for one supersampled canvas."""
    s = size * SS
    side = s * GRID
    lanes = lanes_for(size)
    node_r = MARK["node_radius"] * side
    lane_h = MARK["lane_height"] * side
    lane_gap = MARK["lane_gap"] * side
    fan_span = MARK["fan_span"] * side

    lengths = [MARK["lane_lengths"][i] * side for i in range(min(lanes, len(MARK["lane_lengths"])))]
    while len(lengths) < lanes:
        lengths.append(lengths[-1])

    block = lanes * lane_h + (lanes - 1) * lane_gap
    mark_w = node_r + fan_span + max(lengths)
    origin = s / 2 - mark_w / 2 + MARK["shift"] * side
    node_cx, node_cy = origin + node_r, s / 2

    node = Image.new("L", (s, s), 0)
    ImageDraw.Draw(node).ellipse(
        [node_cx - node_r, node_cy - node_r, node_cx + node_r, node_cy + node_r],
        fill=round(255 * MARK_ALPHA),
    )

    lanes_img = Image.new("L", (s, s), 0)
    top = s / 2 - block / 2
    for i, length in enumerate(lengths):
        cy = top + i * (lane_h + lane_gap) + lane_h / 2
        # The whole lane — curve and straight run — is one stroked path, so the
        # two halves cannot show a seam. Its start is hidden under the node.
        path = lane_path(fan_span, (node_cx, node_cy), cy, node_cx + fan_span + length)
        stamp_path(lanes_img, path, lane_h / 2)

    if MARK["tip_fade"] < 1.0:
        # Fade towards the tips as one ramp over the finished mask: fading each
        # stamp separately would band where the stamps overlap.
        _, x = np.mgrid[0:s, 0:s].astype(np.float32)
        t = np.clip((x - (node_cx + fan_span)) / max(1.0, max(lengths)), 0, 1)
        ramp = 1.0 - t * (1.0 - MARK["tip_fade"])
        lanes_img = Image.fromarray((np.asarray(lanes_img, np.float32) * ramp).astype(np.uint8))
    return node, lanes_img


def to_unit(image: Image.Image) -> np.ndarray:
    return np.asarray(image, np.float32) / 255.0


def blurred(image: Image.Image, s: int, radius: float) -> np.ndarray:
    return to_unit(image.filter(ImageFilter.GaussianBlur(s * radius)))


def render(size: int) -> Image.Image:
    """Draw the icon at `size` pixels, anti-aliased."""
    s = size * SS
    rgb, alpha = canvas(size)
    node_img, lanes_img = mark_masks(size)

    # A soft shadow under the mark, so the mark sits on the surface rather than
    # being painted into it. It is only ever visible inside the squircle.
    shadow = blurred(node_img, s, 0.030) + blurred(lanes_img, s, 0.030)
    shadow = np.roll(shadow, int(s * 0.006), axis=0)
    shadow = np.roll(shadow, int(s * 0.004), axis=1)
    rgb = rgb * (1 - (shadow * MARK_SHADOW_ALPHA)[..., None])

    rgba = np.zeros((s, s, 4), np.float32)
    rgba[..., :3] = rgb
    rgba[..., 3] = alpha
    # A halo under the node, then the lanes, then the node on top.
    glow = blurred(node_img, s, 0.035) * GLOW_ALPHA
    for coverage in (glow, to_unit(lanes_img), to_unit(node_img)):
        rgba[..., :3] = rgba[..., :3] * (1 - coverage[..., None]) + coverage[..., None]
    rgba[..., 3] *= alpha

    image = Image.fromarray((np.clip(rgba, 0, 1) * 255 + 0.5).astype(np.uint8))
    return image.resize((size, size), Image.LANCZOS)


# The names `iconutil` expects: (file, pixel size).
ICONSET = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    default_out = Path(__file__).resolve().parent
    parser.add_argument("--out", type=Path, default=default_out, help=f"where to write (default {default_out})")
    parser.add_argument("--no-icns", action="store_true", help="skip iconutil (not on macOS)")
    args = parser.parse_args()
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)

    cache: dict[int, Image.Image] = {}

    def draw(size: int) -> Image.Image:
        if size not in cache:
            cache[size] = render(size)
        return cache[size]

    master = draw(1024)
    master.save(out / "icon-1024.png")
    print(f"{out / 'icon-1024.png'}")

    iconset = out / "icon.iconset"
    if iconset.is_dir():
        for old in iconset.glob("*.png"):
            old.unlink()
    iconset.mkdir(exist_ok=True)
    for name, size in ICONSET:
        draw(size).save(iconset / name)
    print(f"{iconset}/ ({len(ICONSET)} sizes)")

    if args.no_icns:
        return 0
    icns = out / "AppIcon.icns"
    try:
        subprocess.run(
            ["iconutil", "--convert", "icns", str(iconset), "--output", str(icns)],
            check=True,
            capture_output=True,
        )
    except FileNotFoundError:
        print("make_icon: iconutil not found; skipped the .icns", file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as e:
        print(f"make_icon: iconutil failed: {e.stderr.decode(errors='replace').strip()}", file=sys.stderr)
        return 1
    print(f"{icns}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
