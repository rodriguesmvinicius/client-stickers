#!/usr/bin/env python3
"""Render oxidezap.svg into the multi-size oxidezap.ico the Windows build embeds.

Each size is rasterised from the vector on its own (rsvg-convert), not scaled
down from the largest frame, so the 16 and 24 pixel frames stay crisp.
Frames are stored as PNG, which every Windows since Vista reads.
"""

import struct
import subprocess
from pathlib import Path

SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def main():
    here = Path(__file__).resolve().parent
    svg = here / "oxidezap.svg"
    frames = [
        subprocess.run(
            ["rsvg-convert", "-w", str(size), "-h", str(size), str(svg)],
            capture_output=True, check=True,
        ).stdout
        for size in SIZES
    ]
    offset = 6 + 16 * len(SIZES)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(SIZES)))
    for size, frame in zip(SIZES, frames):
        # A 256 pixel frame is written as 0 in the one-byte size fields.
        side = size % 256
        directory.extend(struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(frame), offset))
        offset += len(frame)
    (here / "oxidezap.ico").write_bytes(bytes(directory) + b"".join(frames))


if __name__ == "__main__":
    main()
