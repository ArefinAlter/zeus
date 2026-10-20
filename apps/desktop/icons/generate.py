"""Reproduce the original vector Z icon as PNG and Windows ICO, without dependencies."""

from pathlib import Path
import struct
import zlib


SIZE = 256
POLYGON = [(60, 64), (196, 64), (196, 96), (108, 160), (196, 160),
           (196, 192), (60, 192), (60, 160), (148, 96), (60, 96)]


def inside_polygon(x, y):
    inside = False
    previous = POLYGON[-1]
    for current in POLYGON:
        ax, ay = previous
        bx, by = current
        if (ay > y) != (by > y) and x < (bx - ax) * (y - ay) / (by - ay) + ax:
            inside = not inside
        previous = current
    return inside


def pixel(x, y):
    if inside_polygon(x, y):
        return (164, 204, 255, 255)
    cx = min(max(x, 48), SIZE - 48)
    cy = min(max(y, 48), SIZE - 48)
    return (16, 29, 45, 255) if (x - cx) ** 2 + (y - cy) ** 2 <= 48 ** 2 else (0, 0, 0, 0)


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def main():
    scanlines = bytearray()
    for y in range(SIZE):
        scanlines.append(0)
        for x in range(SIZE):
            scanlines.extend(pixel(x + 0.5, y + 0.5))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(scanlines, 9)) + chunk(b"IEND", b""))
    root = Path(__file__).parent
    (root / "icon.png").write_bytes(png)
    # A 256px ICO directory encodes width/height as zero; PNG payload is supported by modern Windows.
    ico = struct.pack("<HHH", 0, 1, 1) + struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22) + png
    (root / "icon.ico").write_bytes(ico)


if __name__ == "__main__":
    main()
