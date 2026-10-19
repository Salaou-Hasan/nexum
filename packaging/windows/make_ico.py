#!/usr/bin/env python3
"""Build the Windows .ico from the project icon.

Windows supports PNG-compressed entries in an .ico file (Vista and later),
so the project icon is embedded as-is instead of being re-encoded. The
result carries the 16, 32, 48 and 256 pixel sizes the shell asks for.

Usage: python packaging/windows/make_ico.py <icon.png> <out.ico>
"""

import struct
import sys
import zlib


def read_png_size(data: bytes) -> tuple[int, int]:
    """Read the width and height out of a PNG header."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("not a PNG file")
    if data[12:16] != b"IHDR":
        raise SystemExit("not a PNG file")
    width, height = struct.unpack(">II", data[16:24])
    return width, height


def build_ico(png: bytes, sizes: list[int]) -> bytes:
    """Build an .ico with one PNG-compressed entry per size."""
    entries = []
    offset = 6 + 16 * len(sizes)
    for size in sizes:
        entries.append(
            struct.pack(
                "<BBBBHHII",
                size if size < 256 else 0,
                size if size < 256 else 0,
                0,  # palette colours
                0,  # reserved
                1,  # colour planes
                32,  # bits per pixel
                len(png),
                offset,
            )
        )
        offset += len(png)
    header = struct.pack("<HHH", 0, 1, len(sizes))
    return header + b"".join(entries) + png


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, "rb") as f:
        data = f.read()
    width, height = read_png_size(data)
    if width != height:
        raise SystemExit(f"the icon must be square, got {width}x{height}")
    sizes = [s for s in (16, 32, 48, 256) if s <= width]
    with open(dst, "wb") as f:
        f.write(build_ico(data, sizes))
    print(f"wrote {dst} ({', '.join(str(s) for s in sizes)} px)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
