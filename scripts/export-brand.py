"""Export the editable Mcli artwork. Only asset authors need Pillow and resvg-py."""

import io
import re
import struct
import xml.etree.ElementTree as ET
from copy import deepcopy
from pathlib import Path

import resvg_py
from PIL import Image


BRAND = Path(__file__).resolve().parents[1] / "assets" / "brand"
SVG_NS = "http://www.w3.org/2000/svg"
XLINK_NS = "http://www.w3.org/1999/xlink"
SVG = "{" + SVG_NS + "}"
HREF = "{" + XLINK_NS + "}href"
ET.register_namespace("", SVG_NS)
ET.register_namespace("xlink", XLINK_NS)
MASTER = ET.parse(BRAND / "mcli-horizontal.svg").getroot()
DEFS = {node.attrib["id"]: node for node in MASTER.find(SVG + "defs")}
SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512, 1024)


def vector(name, width, height, layers, *, save=True):
    """Copy just the named shapes and paint servers referenced by this artwork."""
    used = set()

    def visit(key):
        if key in used:
            return
        used.add(key)
        for node in DEFS[key].iter():
            for value in node.attrib.values():
                for ref in re.findall(r"url\(#([\w-]+)\)", value):
                    visit(ref)
            if HREF in node.attrib:
                visit(node.attrib[HREF][1:])

    root = ET.Element(SVG + "svg", width=str(width), height=str(height),
                      viewBox=f"0 0 {width} {height}", role="img")
    ET.SubElement(root, SVG + "title").text = "Mcli"
    defs = ET.SubElement(root, SVG + "defs")
    for key, attrs in layers:
        visit(key)
        ET.SubElement(root, SVG + "use", {HREF: "#" + key, **attrs})
    defs.extend(deepcopy(node) for key, node in DEFS.items() if key in used)
    ET.indent(root, space="  ")
    data = ET.tostring(root, encoding="unicode") + "\n"
    if save:
        (BRAND / f"mcli-{name}.svg").write_text(data, encoding="utf-8", newline="\n")
    return data


def raster(svg, width, height):
    data = resvg_py.svg_to_bytes(svg_string=svg, width=width, height=height,
                                 skip_system_fonts=True)
    return Image.open(io.BytesIO(data)).convert("RGBA")


def dib(image):
    """Windows icon DIB, straight BGRA and a DWORD-aligned 1-bit transparency mask."""
    width, height = image.size
    flipped = image.transpose(Image.Transpose.FLIP_TOP_BOTTOM)
    mask = bytearray()
    stride = ((width + 31) // 32) * 4
    for y in range(height):
        row = bytearray(stride)
        for x in range(width):
            if flipped.getpixel((x, y))[3] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        mask.extend(row)
    pixels = flipped.tobytes("raw", "BGRA")
    header = struct.pack("<IiiHHIIiiII", 40, width, height * 2, 1, 32, 0,
                         len(pixels), 0, 0, 0, 0)
    return header + pixels + mask


def write_ico(frames):
    directory = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    offset = 6 + 16 * len(frames)
    for size, data in frames:
        directory.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0,
                                     1, 32, len(data), offset))
        offset += len(data)
    (BRAND / "mcli.ico").write_bytes(directory + b"".join(data for _, data in frames))


def main():
    vertical = vector("vertical", 184, 268, [
        ("mark", {}), ("wordmark", {"transform": "translate(0 191)"})])
    vector("wordmark", 186, 78, [("wordmark", {})])
    vector("mark", 184, 176, [("mark", {})])
    for name, color in [("mono-white", "#f8fafc"), ("mono-dark", "#0b1220")]:
        transform = "translate(207 50) scale(1.08)"
        vector(name, 410, 176, [("mono-mark", {"color": color}),
                               ("letters", {"fill": color, "transform": transform}),
                               ("i-dot", {"fill": color, "transform": transform})])
    app = vector("app", 256, 256, [("app-art", {})])
    medium = vector("medium", 256, 256, [("medium-app", {})], save=False)
    small = vector("small", 16, 16, [("small-app", {})], save=False)
    png = BRAND / "png"
    png.mkdir(exist_ok=True)
    # Transparent lockups are exported separately from the square desktop icons.
    raster(vertical, 138, 201).save(png / "mcli-vertical.png", optimize=True)
    caption = vector("caption", 96, 32, [
        ("mark", {"transform": "translate(0 1.7) scale(.174 .16)"}),
        ("wordmark", {"transform": "translate(46.5 5.5) scale(.267)"})], save=False)
    raster(caption, 288, 96).save(png / "mcli-caption.png", optimize=True)
    frames = []
    for size in SIZES:
        artwork = app
        if size <= 32:
            artwork = small
        elif size <= 128:
            artwork = medium
        image = raster(artwork, size, size)
        path = png / f"mcli-{size}.png"
        image.save(path, optimize=True)
        if size <= 256:
            frames.append((size, path.read_bytes() if size == 256 else dib(image)))
    write_ico(frames)


if __name__ == "__main__":
    main()
