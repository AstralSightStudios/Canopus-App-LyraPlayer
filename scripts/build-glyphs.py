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

# name -> (source svg, edge in pixels); the small variants are the mini player's
SIZES = {
    "play": ("play", 44), "pause": ("pause", 44), "prev": ("prev", 34), "next": ("next", 34),
    "back": ("back", 24), "repeat": ("repeat", 26), "repeat-one": ("repeat-one", 26), "shuffle": ("shuffle", 26), "once": ("once", 26),
    "minus": ("minus", 20), "plus": ("plus", 20), "note": ("note", 48),
    "play-s": ("play", 28), "pause-s": ("pause", 28), "note-s": ("note", 26),
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
        for name, (source, edge) in SIZES.items():
            png = Path(tmp) / f"{name}.png"
            subprocess.run(["rsvg-convert", "-w", str(edge), "-h", str(edge), str(SOURCE / f"{source}.svg"),
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
