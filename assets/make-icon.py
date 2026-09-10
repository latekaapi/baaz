#!/usr/bin/env python3
"""Draw the placeholder Harness icon: a flat dark rounded tile with a white H.

Stdlib only (zlib + struct emit the PNG by hand), so it runs anywhere:
    python3 assets/make-icon.py [output]

Defaults to assets/icon-1024.png. No downloads, no model, no server.
"""

import struct
import sys
import zlib
from pathlib import Path

SIZE = 1024
RADIUS = 232
# Tile and glyph fills, as (r, g, b).
TILE = (46, 49, 56)
GLYPH = (242, 243, 245)
# The H, in pixels: two bars joined by a crossbar.
BAR_W = 92
BAR_TOP = 302
BAR_BOT = 722
BAR_LEFT_X = 352
BAR_RIGHT_X = 580
CROSS_TOP = 482
CROSS_BOT = 542


def inside_rounded(x: int, y: int) -> bool:
    """True when (x, y) is inside the rounded tile."""
    if RADIUS <= x < SIZE - RADIUS:
        return True
    if RADIUS <= y < SIZE - RADIUS:
        return True
    cx = min(max(x, RADIUS), SIZE - 1 - RADIUS)
    cy = min(max(y, RADIUS), SIZE - 1 - RADIUS)
    return (x - cx) ** 2 + (y - cy) ** 2 < RADIUS**2


def inside_h(x: int, y: int) -> bool:
    """True when (x, y) is part of the H glyph."""
    if not BAR_TOP <= y < BAR_BOT:
        return False
    if BAR_LEFT_X <= x < BAR_LEFT_X + BAR_W:
        return True
    if BAR_RIGHT_X <= x < BAR_RIGHT_X + BAR_W:
        return True
    return (
        BAR_LEFT_X <= x < BAR_RIGHT_X + BAR_W and CROSS_TOP <= y < CROSS_BOT
    )


def main() -> None:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent / "icon-1024.png"
    raw = bytearray()
    for y in range(SIZE):
        raw.append(0)  # filter byte: none
        for x in range(SIZE):
            if inside_h(x, y):
                raw += bytes((*GLYPH, 255))
            elif inside_rounded(x, y):
                raw += bytes((*TILE, 255))
            else:
                raw += b"\x00\x00\x00\x00"
    blob = zlib.compress(bytes(raw), 9)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
        )

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", blob) + chunk(b"IEND", b"")
    out.write_bytes(png)
    print(f"icon: {out} ({len(png)} bytes)")


if __name__ == "__main__":
    main()
