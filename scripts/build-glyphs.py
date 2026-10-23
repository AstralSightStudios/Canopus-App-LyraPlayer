#!/usr/bin/env python3
"""Rasterise assets/glyphs/*.svg into the LVGL v9 ARGB8888 BINs the device embeds.

    scripts/build-glyphs.py            writes crates/lyra-player-device/assets/glyph-<name>.bin
    scripts/build-glyphs.py --sheet P  also writes a PNG contact sheet to P (white on dark)

Needs `rsvg-convert` (librsvg). The pixel size of every glyph is fixed here because the device
places the bitmaps 1:1.
"""
import argparse
import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets/glyphs"
OUT = ROOT / "crates/lyra-player-device/assets"

# name -> edge in pixels
SIZES = {
    "play": 36, "pause": 36, "prev": 28, "next": 28, "back": 24,
    "repeat": 26, "shuffle": 26, "once": 26, "minus": 20, "plus": 20, "note": 48,
}


def load_encoder():
    spec = importlib.util.spec_from_file_location("encode_lvgl_icon", ROOT / "scripts/encode-lvgl-icon.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sheet")
    args = parser.parse_args()
    encoder = load_encoder()
    OUT.mkdir(parents=True, exist_ok=True)
    rendered = []
    with tempfile.TemporaryDirectory() as tmp:
        for name, edge in SIZES.items():
            png = Path(tmp) / f"{name}.png"
            subprocess.run(["rsvg-convert", "-w", str(edge), "-h", str(edge), str(SOURCE / f"{name}.svg"),
                            "-o", str(png)], check=True)
            encoder.convert(png, OUT / f"glyph-{name}.bin")
            rendered.append(png)
        if args.sheet:
            sheet(rendered, Path(args.sheet))
    print("wrote", len(SIZES), "glyphs to", OUT)


def sheet(pngs, path):
    """A contact sheet through ffmpeg: every glyph over a dark background, scaled x4."""
    inputs, filters = [], []
    for index, png in enumerate(pngs):
        inputs += ["-i", str(png)]
        filters.append(f"[{index}:v]format=rgba,scale=iw*4:ih*4:flags=neighbor,pad=224:224:(ow-iw)/2:(oh-ih)/2:color=0x00000000[g{index}]")
    n = len(pngs)
    stack = "".join(f"[g{i}]" for i in range(n))
    graph = ";".join(filters) + f";{stack}hstack=inputs={n}[row];color=c=0x303848:s={n * 224 + 20}x244,format=rgba[bg];[bg][row]overlay=10:10"
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs, "-filter_complex", graph, "-frames:v", "1",
                    str(path)], check=True)


if __name__ == "__main__":
    sys.exit(main())
