#!/usr/bin/env python3
"""Render oxidezap.svg into the multi-size oxidezap.ico the Windows build embeds.

Each size is rasterised from the vector on its own (rsvg-convert), not scaled
down from the largest frame, so the 16 and 24 pixel frames stay crisp.
Frames are stored as PNG, which every Windows since Vista reads.

With --tray it also writes crates/daemon/src/tray/app_icon.rs, the same icon
at 32x32 as raw RGBA for the notification area (needs Pillow).
"""

import io
import struct
import subprocess
import sys
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
    if "--tray" in sys.argv[1:]:
        write_tray(here, frames[SIZES.index(32)])


def write_tray(here, png):
    from PIL import Image

    data = Image.open(io.BytesIO(png)).convert("RGBA").tobytes()
    lines = [
        "    " + ", ".join(f"0x{b:02X}" for b in data[k : k + 32]) + ","
        for k in range(0, len(data), 32)
    ]
    header = (
        "//! The application's own icon at the tray's size, as straight RGBA.\n"
        "//!\n"
        "//! Generated from `packaging/windows/oxidezap.svg` at 32\u00d732 by\n"
        "//! `packaging/windows/make-icon.py --tray`; regenerate rather than edit. Kept\n"
        "//! as bytes so the daemon needs no image decoder for its one picture.\n"
        "\n"
        "/// 32\u00d732 pixels, four bytes each, rows top to bottom.\n"
        "#[rustfmt::skip]\n"
        "pub const APP_ICON: [u8; 4096] = [\n"
    )
    target = here.parents[1] / "crates" / "daemon" / "src" / "tray" / "app_icon.rs"
    target.write_text(header + "\n".join(lines) + "\n];\n", encoding="utf-8")


if __name__ == "__main__":
    main()
