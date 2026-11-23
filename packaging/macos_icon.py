"""Package the vector-derived Metro app image as a native macOS icon."""

from pathlib import Path
import struct


ICON_NAME = "Metro.icns"


def outer_icon_bytes(png_path):
    png = png_path.read_bytes()
    if (png[:24] != b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR" + struct.pack(">II", 256, 256)
            or not png.endswith(b"\x00\x00\x00\x00IEND\xaeB`\x82")):
        raise ValueError(f"{png_path}: expected a complete 256×256 PNG")
    # ic08 is the PNG-backed 256×256 icon representation. Copy the exact
    # canonical pixels instead of adding a renderer or image tool to CI.
    image = struct.pack(">4sI", b"ic08", 8 + len(png)) + png
    return struct.pack(">4sI", b"icns", 8 + len(image)) + image


def write_outer_icon(contents, png_path):
    dest = Path(contents) / "Resources" / ICON_NAME
    icon = outer_icon_bytes(png_path)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(icon)
    dest.chmod(0o644)
    return ICON_NAME
