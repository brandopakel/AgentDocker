#!/usr/bin/env python3
"""Render the AgentDocker app icon from the mark.

The mark — an isometric cube with an A on one face and a D on the
other — is artwork, not something a script can draw, so it lives in the
repository as `docs/images/agentdocker-mark.png` and this composes the
icon around it. What the script owns is the *geometry*: the tile, the
margin, the corner radius, and the resampling down to each size Apple
asks for. Those are the parts that are easy to get subtly wrong and
tedious to check by eye, which is exactly what belongs in code.

    python3 scripts/icon.py out/dir

writes every size an `.iconset` needs, plus `icon-1024.png`.
Dependencies: python3. `scripts/png.py` does the reading and writing.
"""

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import png  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
MARK = os.path.join(HERE, os.pardir, "docs", "images", "agentdocker-mark.png")

# The tile, sampled from the artwork's own ground so the icon and the
# mark are lit the same way.
TOP = (0x0D, 0x1B, 0x2C)
BOTTOM = (0x05, 0x0A, 0x14)

# Apple's macOS icon grid, from the Big Sur template. On a 1024pt canvas
# the rounded-rectangle body is 824pt square and centred, with a corner
# radius of 185.4pt; the 100pt of margin all round is what keeps every
# icon in the Dock the same visual size. An icon drawn edge to edge
# renders about a quarter larger than its neighbours.
BODY = 824.0 / 1024.0
CORNER = 185.4 / 824.0  # of the body's side, not the canvas

# How much of the body the mark fills. Short of the corners, because a
# mark that reaches them reads as cramped — at the sizes where there is
# room to notice.
MARK_FILL = 0.80

# Below this the margin is costing more than it buys. At 16pt the body
# is thirteen pixels across and the mark inside it is ten; every pixel
# spent on breathing room is a pixel not spent on the shape, and what
# survives is a blue smudge. Apple's own guidance is to simplify at
# small sizes rather than to reduce, and filling the tile is the
# simplification available to a mark that cannot be redrawn.
TIGHT_BELOW = 40
TIGHT_FILL = 0.96


def fill_for(size):
    return TIGHT_FILL if size < TIGHT_BELOW else MARK_FILL


def smooth(distance, softness):
    """Coverage from a signed distance: 1 inside, 0 outside, soft edge."""
    t = 0.5 - distance / softness
    if t <= 0.0:
        return 0.0
    if t >= 1.0:
        return 1.0
    return t * t * (3.0 - 2.0 * t)


def rounded_rect(x, y, half, radius):
    """Signed distance to a rounded square of half-width `half`, centred."""
    dx = abs(x - 0.5) - (half - radius)
    dy = abs(y - 0.5) - (half - radius)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    return outside + min(max(dx, dy), 0.0) - radius


def tile(size):
    """The rounded, gradient-filled body, as rows of RGBA."""
    edge = 1.0 / size
    half = BODY / 2.0
    corner = CORNER * BODY
    rows = []
    for py in range(size):
        y = (py + 0.5) / size
        ramp = min(max((y - (0.5 - half)) / BODY, 0.0), 1.0)
        ground = tuple(round(t + (b - t) * ramp) for t, b in zip(TOP, BOTTOM))
        row = bytearray(size * 4)
        for px in range(size):
            x = (px + 0.5) / size
            cover = smooth(rounded_rect(x, y, half, corner), edge)
            if cover <= 0.0:
                continue
            at = px * 4
            row[at], row[at + 1], row[at + 2] = ground
            row[at + 3] = round(cover * 255)
        rows.append(row)
    return rows


def place(base, size, mark_w, mark_h, mark_rows):
    """Composite the mark over the tile, centred, at MARK_FILL of the body."""
    room = int(size * BODY * fill_for(size))
    if room < 1:
        return base
    scale = min(room / mark_w, room / mark_h)
    target_w = max(1, round(mark_w * scale))
    target_h = max(1, round(mark_h * scale))
    scaled = png.resample(mark_rows, mark_w, mark_h, target_w, target_h)
    left, top = (size - target_w) // 2, (size - target_h) // 2
    for y, line in enumerate(scaled):
        dest = base[top + y]
        for x in range(target_w):
            at, to = x * 4, (left + x) * 4
            alpha = line[at + 3]
            if not alpha:
                continue
            for c in range(3):
                dest[to + c] = (
                    line[at + c] * alpha + dest[to + c] * (255 - alpha)
                ) // 255
            dest[to + 3] = max(dest[to + 3], alpha)
    return base


def draw(size, mark):
    mark_w, mark_h, mark_rows = mark
    return place(tile(size), size, mark_w, mark_h, mark_rows)


# What an `.iconset` must contain for `iconutil` to accept it.
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


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "build/icon"
    mark = png.read(MARK)
    iconset = os.path.join(out, "AgentDocker.iconset")
    os.makedirs(iconset, exist_ok=True)
    drawn = {}
    for name, size in ICONSET:
        if size not in drawn:
            drawn[size] = draw(size, mark)
        png.write(os.path.join(iconset, name), size, drawn[size])
        print(f"{name} ({size}px)")
    png.write(os.path.join(out, "icon-1024.png"), 1024, drawn[1024])
    print("icon-1024.png")


if __name__ == "__main__":
    main()
