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
16 px icon is a 16 px drawing, not a thumbnail of the 1024. Below
`MARK["small_size"]` the strokes thicken, which is the difference between grey
hairlines and a legible letter at 16 and 32 px.

The shape is Apple's Big Sur icon grid: a squircle 824/1024 of the canvas,
drawn as a superellipse (a plain rounded rectangle reads as a circle-cornered
square next to real macOS icons). The mark is a lowercase e drawn as a taiji
(☯): a round bowl, open at the lower right, whose crossbar is the wave that
divides yin from yang. The crossbar and the half of the bowl above it are
yang, solid white; the half below is yin, the same white at low opacity. Both
are round-capped strokes of one width, and the bowl's upper half ends in the
crossbar's right-hand cap, so the letter reads as one continuous line.

Needs Python 3 with Pillow and NumPy. The .icns is packed by `iconutil` on
macOS, and by `write_icns` below anywhere else.
"""

from __future__ import annotations

import argparse
import math
import struct
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

# The mark is white: yang solid, yin translucent, and a soft shadow under the
# whole thing.
YANG_ALPHA = 1.0
YIN_ALPHA = 0.45
MARK_SHADOW_ALPHA = 0.16

# --- the mark ---------------------------------------------------------------
# Every number is a fraction of the squircle's side, except where noted.
MARK = {
    "radius": 0.30,         # the bowl's outer edge
    "stroke": 0.095,        # one width for the bowl and the crossbar
    # The crossbar's wave, as a fraction of the bowl's centre-line radius: it
    # dips on the left and rises on the right, the way the taiji's S does, but
    # as a sine rather than two semicircles, whose vertical join would read as
    # a kink in a crossbar.
    "wave": 0.30,
    # Where the bowl stops, in degrees clockwise from 3 o'clock: the e's mouth
    # runs from the crossbar's right-hand end (0) down to here.
    "mouth": 50.0,
    # Below this many pixels the strokes thicken to this, or they go grey.
    "small_size": 48,
    "small_stroke": 0.12,
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


def stroke_for(size: int) -> float:
    """The stroke width for this size, as a fraction of the squircle's side."""
    return MARK["stroke"] if size >= MARK["small_size"] else MARK["small_stroke"]


def wave_y(x, radius: float, amplitude: float):
    """The crossbar's height at `x` (relative to the centre): a sine across the
    bowl, below the axis on the left and above it on the right."""
    return amplitude * np.sin(-np.pi * np.clip(x, -radius, radius) / radius)


def mark_masks(size: int) -> tuple[Image.Image, Image.Image]:
    """`(yang, yin)` coverage masks for one supersampled canvas."""
    s = size * SS
    side = s * GRID
    c = s / 2
    stroke = stroke_for(size) * side
    # The strokes' centre line: the bowl's outer edge is at MARK["radius"].
    r = MARK["radius"] * side - stroke / 2
    amplitude = MARK["wave"] * r

    steps = 720
    bar = [(c + x, c + float(wave_y(x, r, amplitude))) for x in np.linspace(-r, r, steps)]
    # From the crossbar's right-hand end, anticlockwise over the top and round
    # the bottom to the mouth.
    sweep = np.radians(np.linspace(0.0, -(360.0 - MARK["mouth"]), steps))
    bowl = [(c + r * math.cos(a), c + r * math.sin(a)) for a in sweep]

    bar_img = Image.new("L", (s, s), 0)
    stamp_path(bar_img, bar, stroke / 2)
    bowl_img = Image.new("L", (s, s), 0)
    stamp_path(bowl_img, bowl, stroke / 2)

    # Yang is the crossbar and whatever of the bowl lies above it; yin is the
    # rest of the bowl. Splitting along the wave, rather than at the bowl's
    # stroke ends, is what makes the two halves meet the way the taiji's do.
    y, x = np.mgrid[0:s, 0:s].astype(np.float32)
    above = (y - c) < wave_y(x - c, r, amplitude)
    bar_a = np.asarray(bar_img) > 127
    bowl_a = np.asarray(bowl_img) > 127
    yang = bar_a | (bowl_a & above)
    yin = bowl_a & ~yang
    return (
        Image.fromarray((yang * round(255 * YANG_ALPHA)).astype(np.uint8)),
        Image.fromarray((yin * round(255 * YIN_ALPHA)).astype(np.uint8)),
    )


def to_unit(image: Image.Image) -> np.ndarray:
    return np.asarray(image, np.float32) / 255.0


def blurred(image: Image.Image, s: int, radius: float) -> np.ndarray:
    return to_unit(image.filter(ImageFilter.GaussianBlur(s * radius)))


def render(size: int) -> Image.Image:
    """Draw the icon at `size` pixels, anti-aliased."""
    s = size * SS
    rgb, alpha = canvas(size)
    yang_img, yin_img = mark_masks(size)

    # A soft shadow under the mark, so the mark sits on the surface rather than
    # being painted into it. It is only ever visible inside the squircle.
    shadow = blurred(yang_img, s, 0.030) + blurred(yin_img, s, 0.030)
    shadow = np.roll(shadow, int(s * 0.006), axis=0)
    shadow = np.roll(shadow, int(s * 0.004), axis=1)
    rgb = rgb * (1 - (shadow * MARK_SHADOW_ALPHA)[..., None])

    rgba = np.zeros((s, s, 4), np.float32)
    rgba[..., :3] = rgb
    rgba[..., 3] = alpha
    for coverage in (to_unit(yin_img), to_unit(yang_img)):
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


# The .icns entry for each iconset file, as `iconutil` writes them.
ICNS_TYPES = {
    "icon_16x16.png": b"ic04",
    "icon_16x16@2x.png": b"ic11",
    "icon_32x32.png": b"ic05",
    "icon_32x32@2x.png": b"ic12",
    "icon_128x128.png": b"ic07",
    "icon_128x128@2x.png": b"ic13",
    "icon_256x256.png": b"ic08",
    "icon_256x256@2x.png": b"ic14",
    "icon_512x512.png": b"ic09",
    "icon_512x512@2x.png": b"ic10",
}


def write_icns(icns: Path, iconset: Path) -> None:
    """Pack `iconset` into `icns` without `iconutil`: an .icns is a header and
    one (type, length, PNG) record per size, all lengths big-endian."""
    records = b""
    for name, _ in ICONSET:
        png = (iconset / name).read_bytes()
        records += ICNS_TYPES[name] + struct.pack(">I", 8 + len(png)) + png
    icns.write_bytes(b"icns" + struct.pack(">I", 8 + len(records)) + records)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    default_out = Path(__file__).resolve().parent
    parser.add_argument("--out", type=Path, default=default_out, help=f"where to write (default {default_out})")
    parser.add_argument("--no-icns", action="store_true", help="skip the .icns")
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
        # Not on macOS: pack the iconset ourselves.
        write_icns(icns, iconset)
    except subprocess.CalledProcessError as e:
        print(f"make_icon: iconutil failed: {e.stderr.decode(errors='replace').strip()}", file=sys.stderr)
        return 1
    print(f"{icns}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
