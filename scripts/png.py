"""Read and write PNGs with nothing but the standard library.

The icon pipeline needs to open the artwork, measure it, resample it and
write it back out at ten sizes. Every library that does this is a
dependency somebody has to install before they can build the app, and
the whole point of `icon.py` is that a checkout plus python3 is enough.
So: enough of PNG to be useful, and no more.

Supports the colour types the artwork actually uses — 8-bit RGB and
RGBA, non-interlaced — and says so plainly when handed anything else,
rather than returning quietly wrong pixels.
"""

import struct
import zlib

CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}


def read(path):
    """`(width, height, rows)`, each row a bytearray of RGBA."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    width = height = depth = colour = None
    palette = b""
    body = bytearray()
    i = 8
    while i < len(data):
        length = struct.unpack(">I", data[i : i + 4])[0]
        kind = data[i + 4 : i + 8]
        chunk = data[i + 8 : i + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if depth != 8:
                raise ValueError(f"{path}: only 8-bit channels are supported")
            if interlace:
                raise ValueError(f"{path}: interlaced PNGs are not supported")
            if colour not in (2, 3, 6):
                raise ValueError(f"{path}: colour type {colour} is not supported")
        elif kind == b"PLTE":
            palette = chunk
        elif kind == b"IDAT":
            body += chunk
        elif kind == b"IEND":
            break
        i += 12 + length

    raw = zlib.decompress(bytes(body))
    stride = CHANNELS[colour] * width
    rows = []
    previous = bytearray(stride)
    at = 0
    for _ in range(height):
        filter_type = raw[at]
        line = bytearray(raw[at + 1 : at + 1 + stride])
        at += 1 + stride
        unfilter(filter_type, line, previous, CHANNELS[colour])
        rows.append(to_rgba(line, colour, palette, width))
        previous = line
    return width, height, rows


def unfilter(kind, line, previous, step):
    """Undo one scanline's filter, in place. The five of them from the spec."""
    if kind == 0:
        return
    for i in range(len(line)):
        left = line[i - step] if i >= step else 0
        up = previous[i]
        if kind == 1:
            line[i] = (line[i] + left) & 0xFF
        elif kind == 2:
            line[i] = (line[i] + up) & 0xFF
        elif kind == 3:
            line[i] = (line[i] + (left + up) // 2) & 0xFF
        elif kind == 4:
            upleft = previous[i - step] if i >= step else 0
            # Paeth: whichever of the three neighbours the gradient
            # predictor lands closest to.
            p = left + up - upleft
            pa, pb, pc = abs(p - left), abs(p - up), abs(p - upleft)
            best = left if (pa <= pb and pa <= pc) else (up if pb <= pc else upleft)
            line[i] = (line[i] + best) & 0xFF
        else:
            raise ValueError(f"unknown scanline filter {kind}")


def to_rgba(line, colour, palette, width):
    if colour == 6:
        return bytearray(line)
    out = bytearray(width * 4)
    if colour == 2:
        for x in range(width):
            out[x * 4 : x * 4 + 3] = line[x * 3 : x * 3 + 3]
            out[x * 4 + 3] = 255
    else:  # indexed
        for x in range(width):
            at = line[x] * 3
            out[x * 4 : x * 4 + 3] = palette[at : at + 3]
            out[x * 4 + 3] = 255
    return out


def write(path, width, rows):
    """A PNG of those RGBA rows. Filter 0 throughout: the artwork
    compresses well enough and a filter is another way to be wrong."""
    raw = b"".join(b"\x00" + bytes(row) for row in rows)

    def chunk(kind, body):
        data = kind + body
        return (
            struct.pack(">I", len(body))
            + data
            + struct.pack(">I", zlib.crc32(data) & 0xFFFFFFFF)
        )

    out = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, len(rows), 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    open(path, "wb").write(out)


def resample(rows, width, height, target_w, target_h):
    """Box-filter down to `target_w` x `target_h`.

    Averaging every source pixel that falls under a destination pixel,
    which is what makes a 1024-pixel mark still legible at 32. Sampling
    one pixel per destination — which is what a naive resize does —
    turns fine edges into noise.
    """
    out = []
    for y in range(target_h):
        y0, y1 = y * height // target_h, max((y + 1) * height // target_h, y * height // target_h + 1)
        row = bytearray(target_w * 4)
        for x in range(target_w):
            x0, x1 = x * width // target_w, max((x + 1) * width // target_w, x * width // target_w + 1)
            r = g = b = a = n = 0
            for sy in range(y0, y1):
                line = rows[sy]
                for sx in range(x0, x1):
                    at = sx * 4
                    alpha = line[at + 3]
                    # Weighted by alpha so transparent pixels do not drag
                    # the colour towards whatever happens to be under them.
                    r += line[at] * alpha
                    g += line[at + 1] * alpha
                    b += line[at + 2] * alpha
                    a += alpha
                    n += 1
            at = x * 4
            if a:
                row[at] = min(255, r // a)
                row[at + 1] = min(255, g // a)
                row[at + 2] = min(255, b // a)
            row[at + 3] = a // n if n else 0
        out.append(row)
    return out
