#!/usr/bin/env python3
"""Draw the AgentDocker mark, at whatever size is asked for.

The mark is a ring with three agents on it: one host, and the agents
docked around it, in the same three colours the app gives the first
three projects it sees. It is drawn here rather than stored as a
rendered file so the shape has a source, the sizes cannot drift apart,
and anyone can regenerate the whole set from a checkout with nothing
installed. Python's standard library writes the PNGs; macOS's own
`iconutil` turns them into the `.icns`.

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

CORNER = 0.2237  # Apple's squircle radius, as a fraction of the side.
RING_R = 0.255
RING_W = 0.052
AGENT_R = 0.088


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


def rounded_rect(x, y, radius):
    """Signed distance to a rounded square filling the unit box."""
    dx = abs(x - 0.5) - (0.5 - radius)
    dy = abs(y - 0.5) - (0.5 - radius)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    return outside + min(max(dx, dy), 0.0) - radius


def draw(size):
    """The mark at `size`x`size`, as rows of RGBA bytes."""
    # One pixel's width in unit space, which is how wide an edge is
    # allowed to be: any narrower and the small sizes go jagged, any
    # wider and the large ones go soft.
    edge = 1.0 / size
    centres = [
        (0.5 + RING_R * math.cos(a), 0.5 + RING_R * math.sin(a))
        for a in (-math.pi / 2, math.pi / 6, 5 * math.pi / 6)
    ]
    rows = []
    for py in range(size):
        y = (py + 0.5) / size
        row = bytearray()
        # The tile's gradient is vertical, so it is the same all along
        # a row.
        ground = tuple(
            round(t + (b - t) * y) for t, b in zip(TOP, BOTTOM)
        )
        for px in range(size):
            x = (px + 0.5) / size
            tile = smooth(rounded_rect(x, y, CORNER), edge)
            if tile <= 0.0:
                row += b"\x00\x00\x00\x00"
                continue
            colour = ground
            # The ring: the outside of an annulus, so one distance.
            ring = abs(math.hypot(x - 0.5, y - 0.5) - RING_R) - RING_W / 2
            colour = over(colour, RING, smooth(ring, edge))
            for (cx, cy), agent in zip(centres, AGENTS):
                dot = math.hypot(x - cx, y - cy) - AGENT_R
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
