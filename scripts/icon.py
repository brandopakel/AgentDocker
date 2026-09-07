#!/usr/bin/env python3
"""Draw the AgentDocker mark, at whatever size is asked for.

The mark is the product: three agents, in the three colours the app
gives the first three projects it sees, coordinating through one host.
Different vendors, different models, one daemon between them — which is
the whole claim, and it is worth a mark that says it rather than the
ring-of-dots every network diagram already uses.

It is drawn here rather than stored as a rendered file so the shape has
a source, the sizes cannot drift apart, and anyone can regenerate the
whole set from a checkout with nothing installed. Python's standard
library writes the PNGs; macOS's own `iconutil` turns them into the
`.icns`.

    python3 scripts/icon.py out/dir

writes every size an `.iconset` needs, plus `icon-1024.png` for the
README and the repository's social image.
"""

import math
import os
import struct
import sys
import zlib

# The tile. Dark, because it sits in a dock beside other dark tiles, and
# because the app itself is dark.
TOP = (0x22, 0x27, 0x31)
BOTTOM = (0x14, 0x17, 0x1D)
RING = (0x8A, 0x93, 0xA5)
# Blue, pink, green: the first three project colours the app assigns.
AGENTS = ((0x5B, 0x8C, 0xFF), (0xFF, 0x5F, 0xA2), (0x3D, 0xDC, 0x84))

# Apple's macOS icon grid, from the Big Sur template. On a 1024pt
# canvas the rounded-rectangle body is 824pt square and centred, with a
# corner radius of 185.4pt; the 100pt of margin all round is what keeps
# every icon in the Dock the same visual size. Drawing the body edge to
# edge — which is the obvious thing to do, and what this did first —
# makes the icon render about a quarter larger than its neighbours.
BODY = 824.0 / 1024.0
CORNER = 185.4 / 824.0  # of the body's side, not the canvas

# Everything inside is a fraction of the body, so the composition holds
# whatever the margin is. The three agents sit in a column on the left,
# the host is the disc on the right, and a connector runs from each.
AGENT_X = 0.26
HOST_X = 0.735
ROWS = (0.235, 0.5, 0.765)
AGENT_R = 0.105
HOST_R = 0.125
LINK_W = 0.040

# No stroke thinner than this many pixels, whatever the size. At 16pt
# the ring is otherwise under a pixel wide and reads as a grey smudge;
# holding a floor is what optical scaling does for line weights.
MIN_STROKE_PX = 1.3

# Below this the mark is drawn simplified. At 16pt the body is thirteen
# pixels across, and three connectors an eighth of that wide converging
# on a disc turn into one grey smudge. So the connectors go and the
# discs grow into the room they leave: three colours on the left, the
# host on the right, which is still the thing the mark is about. Apple's
# own guidance is to simplify at small sizes rather than to reduce.
SIMPLIFY_BELOW_PX = 24
SMALL_AGENT_R = 0.125
SMALL_HOST_R = 0.135


def smooth(distance, softness):
    """Coverage from a signed distance: 1 inside, 0 outside, soft edge."""
    t = 0.5 - distance / softness
    if t <= 0.0:
        return 0.0
    if t >= 1.0:
        return 1.0
    return t * t * (3.0 - 2.0 * t)


def over(under, above, alpha):
    """Composite `above` onto `under` at `alpha`."""
    if alpha <= 0.0:
        return under
    if alpha >= 1.0:
        return above
    return tuple(round(u + (a - u) * alpha) for u, a in zip(under, above))


def capsule(x, y, x0, y0, x1, y1, radius):
    """Signed distance to a rounded line from (x0,y0) to (x1,y1)."""
    px, py = x - x0, y - y0
    bx, by = x1 - x0, y1 - y0
    length = bx * bx + by * by
    along = 0.0 if length == 0.0 else max(0.0, min(1.0, (px * bx + py * by) / length))
    return math.hypot(px - bx * along, py - by * along) - radius


def body_space(t):
    """A fraction of the body, as a fraction of the canvas."""
    return 0.5 + (t - 0.5) * BODY


def rounded_rect(x, y, half, radius):
    """Signed distance to a rounded square of half-width `half`, centred."""
    dx = abs(x - 0.5) - (half - radius)
    dy = abs(y - 0.5) - (half - radius)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    return outside + min(max(dx, dy), 0.0) - radius


def draw(size):
    """The mark at `size`x`size`, as rows of RGBA bytes."""
    # One pixel's width in unit space, which is how wide an edge is
    # allowed to be: any narrower and the small sizes go jagged, any
    # wider and the large ones go soft.
    edge = 1.0 / size
    small = size < SIMPLIFY_BELOW_PX
    half = BODY / 2.0
    corner = CORNER * BODY
    agent_r = (SMALL_AGENT_R if small else AGENT_R) * BODY
    host_r = (SMALL_HOST_R if small else HOST_R) * BODY
    link_w = 0.0 if small else max(LINK_W * BODY, MIN_STROKE_PX / size)
    host = (body_space(HOST_X), body_space(0.5))
    centres = [(body_space(AGENT_X), body_space(row)) for row in ROWS]
    rows = []
    for py in range(size):
        y = (py + 0.5) / size
        row = bytearray()
        # The tile's gradient is vertical, so it is the same all along
        # a row.
        ramp = min(max((y - (0.5 - half)) / BODY, 0.0), 1.0)
        ground = tuple(round(t + (b - t) * ramp) for t, b in zip(TOP, BOTTOM))
        for px in range(size):
            x = (px + 0.5) / size
            tile = smooth(rounded_rect(x, y, half, corner), edge)
            if tile <= 0.0:
                row += b"\x00\x00\x00\x00"
                continue
            colour = ground
            # Connectors first, so the discs sit on top of them.
            if link_w > 0.0:
                for cx, cy in centres:
                    link = capsule(x, y, cx, cy, host[0], host[1], link_w)
                    colour = over(colour, RING, smooth(link, edge))
            colour = over(
                colour,
                RING,
                smooth(math.hypot(x - host[0], y - host[1]) - host_r, edge),
            )
            for (cx, cy), agent in zip(centres, AGENTS):
                dot = math.hypot(x - cx, y - cy) - agent_r
                colour = over(colour, agent, smooth(dot, edge))
            row += bytes(colour) + bytes((round(tile * 255),))
        rows.append(bytes(row))
    return rows


def png(rows, size):
    """A PNG of those rows. No filtering: the shapes are smooth and the
    files are small enough that a filter would only add a way to be
    wrong."""
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(kind, body):
        data = kind + body
        return struct.pack(">I", len(body)) + data + struct.pack(
            ">I", zlib.crc32(data) & 0xFFFFFFFF
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


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
    iconset = os.path.join(out, "AgentDocker.iconset")
    os.makedirs(iconset, exist_ok=True)
    # Each size is drawn once and reused wherever the set repeats it.
    drawn = {}
    for name, size in ICONSET:
        if size not in drawn:
            drawn[size] = png(draw(size), size)
        with open(os.path.join(iconset, name), "wb") as f:
            f.write(drawn[size])
        print(f"{name} ({size}px)")
    with open(os.path.join(out, "icon-1024.png"), "wb") as f:
        f.write(drawn[1024])
    print("icon-1024.png")


if __name__ == "__main__":
    main()
