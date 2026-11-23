"""Build Metro's logo exports from the UI mark and pinned Inter. No network.

Authoring dependencies: fontTools, PyGObject/GdkPixbuf with the SVG loader.
Generated assets are checked in; building or installing Metro does not run this.
"""
from pathlib import Path
import hashlib
import struct
import xml.etree.ElementTree as ET

import gi
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf

APP = Path(__file__).resolve().parents[1]
ICONS = APP / "src/ui/icons"
ASSETS = APP / "assets"
BLUE = "#0A58FF"
MARK = ET.parse(ICONS / "metro.svg").getroot().find("{http://www.w3.org/2000/svg}path").attrib["d"]


def svg(width, height, content):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}">\n  <title>Metro</title>\n  {content}\n</svg>\n')


def outline():
    # Paths, not a live SVG font: Explorer, Finder and native UI see the
    # same lowercase wordmark. Only the logo uses tighter optical spacing.
    source = APP / "src/ui/fonts/InterVariable.ttf"
    assert hashlib.sha256(source.read_bytes()).hexdigest() == "29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031"
    font = TTFont(source)
    glyphs = font.getGlyphSet(location={"wght": 760, "opsz": 32})
    pen = SVGPathPen(glyphs, ntos=lambda n: f"{n:.1f}".rstrip("0").rstrip("."))
    bounds = BoundsPen(glyphs)
    x = 0
    for char in "metro":
        glyph = glyphs[font.getBestCmap()[ord(char)]]
        transform = (1, 0, 0, -1, x, 0)
        for p in (pen, bounds):
            glyph.draw(TransformPen(p, transform))
        x += glyph.width - font["head"].unitsPerEm * 0.035
    left, top, right, bottom = bounds.bounds
    return (f'<g transform="scale({272 / (right - left):.7f} {76 / (bottom - top):.7f}) '
            f'translate({-left:.2f} {-top:.2f})"><path d="{pen.getCommands()}"/></g>')


def png_bytes(path, size):
    image = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(path), size, size, True)
    ok, data = image.save_to_bufferv("png", [], [])
    assert ok and image.get_width() == image.get_height() == size
    return bytes(data)


def main():
    word = outline()
    mark = f'<path d="{MARK}"/>'
    horizontal = f'<g transform="translate(0 2)">{mark}</g><g transform="translate(226 5)">{word}</g>'
    stacked = f'<g transform="translate(36 2)">{mark}</g><g transform="translate(0 106)">{word}</g>'
    brand = ASSETS / "brand"
    brand.mkdir(exist_ok=True)
    # The UI tints the transparent lockup against the active theme. Plate
    # exports include a quarter-mark-height of clear space on every side.
    (ICONS / "metro-horizontal.svg").write_text(svg(498, 96, horizontal))
    (brand / "metro-stacked.svg").write_text(svg(272, 184, stacked))
    for name, background in [("dark", "#0B0F19"), ("blue", BLUE)]:
        (brand / f"metro-{name}.svg").write_text(svg(546, 144,
            f'<rect width="546" height="144" rx="12" fill="{background}"/>'
            f'<g fill="#fff" transform="translate(24 24)">{horizontal}</g>'))

    app = ASSETS / "metro.svg"
    app.write_text(svg(256, 256, f'<rect x="4" y="4" width="248" height="248" rx="52" fill="{BLUE}"/>'
                       f'<g fill="#fff" transform="translate(23 78) scale(1.06)">{mark}</g>'))
    (ASSETS / "metro-256.png").write_bytes(png_bytes(app, 256))
    sizes = (16, 24, 32, 48, 64, 128, 256)
    frames = [png_bytes(app, size) for size in sizes]
    directory = bytearray(struct.pack("<HHH", 0, 1, len(sizes)))
    offset = 6 + 16 * len(sizes)
    for size, data in zip(sizes, frames):
        directory.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    (ASSETS / "metro.ico").write_bytes(directory + b"".join(frames))


if __name__ == "__main__":
    main()
