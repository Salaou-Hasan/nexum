#!/usr/bin/env python3
"""Build the Windows .ico from the project icon.

The icon is written in the classic format: one uncompressed 32-bit bitmap
per size, so every Windows version can show it (a PNG-compressed entry
works on the shell, but not in every program that draws an icon).

The PNG is decoded here, so the script needs nothing but the standard
library.

Usage: python packaging/windows/make_ico.py <icon.png> <out.ico>
"""

import struct
import sys
import zlib

SIZES = (16, 24, 32, 48, 256)


def read_png(data: bytes) -> tuple[int, int, list[bytearray]]:
    """Decode a PNG into its width, height and rows of RGBA pixels."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("not a PNG file")
    pos = 8
    width = height = 0
    bit_depth = color_type = 0
    idat = bytearray()
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, bit_depth, color_type = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if bit_depth != 8 or color_type not in (6, 2):
        raise SystemExit(f"unsupported PNG (depth {bit_depth}, type {color_type})")
    channels = 4 if color_type == 6 else 3
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    rows: list[bytearray] = []
    previous = bytearray(stride)
    pos = 0
    for _ in range(height):
        filter_type = raw[pos]
        pos += 1
        line = bytearray(raw[pos : pos + stride])
        pos += stride
        for i in range(stride):
            a = line[i - channels] if i >= channels else 0
            b = previous[i]
            c = previous[i - channels] if i >= channels else 0
            x = line[i]
            if filter_type == 1:
                x = (x + a) & 0xFF
            elif filter_type == 2:
                x = (x + b) & 0xFF
            elif filter_type == 3:
                x = (x + (a + b) // 2) & 0xFF
            elif filter_type == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                x = (x + pred) & 0xFF
            line[i] = x
        rows.append(line)
        previous = line
    # Normalise to RGBA.
    pixels: list[bytearray] = []
    for line in rows:
        out = bytearray(width * 4)
        for x in range(width):
            r, g, b = line[x * channels], line[x * channels + 1], line[x * channels + 2]
            a = line[x * channels + 3] if channels == 4 else 255
            out[x * 4 : x * 4 + 4] = bytes((r, g, b, a))
        pixels.append(out)
    return width, height, pixels


def scale(pixels: list[bytearray], width: int, height: int, size: int):
    """Scale the image down to `size` pixels, with the alpha channel."""
    out = []
    for y in range(size):
        row = bytearray(size * 4)
        sy = min(height - 1, y * height // size)
        source = pixels[sy]
        for x in range(size):
            sx = min(width - 1, x * width // size)
            row[x * 4 : x * 4 + 4] = source[sx * 4 : sx * 4 + 4]
        out.append(row)
    return out


def dib(size: int, pixels: list[bytearray]) -> bytes:
    """The image of one icon entry: a DIB with its alpha mask."""
    header = struct.pack(
        "<IiiHHIIiiII",
        40,  # header size
        size,  # width
        size * 2,  # height (the image and the mask)
        1,  # planes
        32,  # bits per pixel
        0,  # compression
        0,  # image size
        0,
        0,
        0,
        0,
    )
    body = bytearray()
    # The rows are stored bottom up, in BGRA.
    for row in reversed(pixels):
        for x in range(size):
            r, g, b, a = row[x * 4 : x * 4 + 4]
            body += bytes((b, g, r, a))
    mask_stride = ((size + 31) // 32) * 4
    mask = bytearray(mask_stride * size)
    return header + bytes(body) + bytes(mask)


def build_ico(images: list[tuple[int, bytes]]) -> bytes:
    """Assemble the .ico from the images."""
    count = len(images)
    offset = 6 + 16 * count
    entries = bytearray()
    payload = bytearray()
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,  # palette colours
            0,  # reserved
            1,  # colour planes
            32,  # bits per pixel
            len(data),
            offset + len(payload),
        )
        payload += data
    return struct.pack("<HHH", 0, 1, count) + bytes(entries) + bytes(payload)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, "rb") as f:
        data = f.read()
    width, height, pixels = read_png(data)
    if width != height:
        raise SystemExit(f"the icon must be square, got {width}x{height}")
    images = []
    for size in SIZES:
        if size > width:
            continue
        images.append((size, dib(size, scale(pixels, width, height, size))))
    with open(dst, "wb") as f:
        f.write(build_ico(images))
    print(f"wrote {dst} ({', '.join(str(s) for s, _ in images)} px)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
