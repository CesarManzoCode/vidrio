#!/usr/bin/env python3
"""Writes expected.png for the first frame test (vault/CONTRACT.md §5.8).

Reads the list of rectangles from page.html (the inline style of each
absolutely positioned div: left, top, width, height and background in
whole pixels and #rrggbb) and paints them in document order over the
white canvas into an 800x600 image, which it writes as an 8-bit RGB PNG
without interlacing. It does not use vidrio.
"""

import pathlib
import re
import struct
import sys
import zlib

WIDTH = 800
HEIGHT = 600
HERE = pathlib.Path(__file__).resolve().parent

DIV = re.compile(
    r'<div style="left: (\d+)px; top: (\d+)px; width: (\d+)px; height: (\d+)px; '
    r'background: #([0-9a-f]{6});"></div>'
)


def rectangles(html):
    """The (left, top, width, height, (r, g, b)) of each box, in order."""
    out = []
    for m in DIV.finditer(html):
        left, top, width, height = (int(m.group(i)) for i in range(1, 5))
        rgb = bytes.fromhex(m.group(5))
        out.append((left, top, width, height, (rgb[0], rgb[1], rgb[2])))
    return out


def paint(rects):
    """Rows of RGB bytes: white, then each rectangle clipped to the image."""
    rows = [bytearray(b"\xff" * (WIDTH * 3)) for _ in range(HEIGHT)]
    for left, top, width, height, rgb in rects:
        x0, x1 = max(left, 0), min(left + width, WIDTH)
        y0, y1 = max(top, 0), min(top + height, HEIGHT)
        if x0 >= x1:
            continue
        span = bytes(rgb) * (x1 - x0)
        for y in range(y0, y1):
            rows[y][x0 * 3 : x1 * 3] = span
    return rows


def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


def png(rows):
    """An 8-bit RGB PNG, filter type 0 on every row, no interlacing."""
    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    rects = rectangles((HERE / "page.html").read_text(encoding="utf-8"))
    if not rects:
        print("page.html has no rectangles", file=sys.stderr)
        return 1
    (HERE / "expected.png").write_bytes(png(paint(rects)))
    print(f"expected.png: {len(rects)} rectangles, {WIDTH}x{HEIGHT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
