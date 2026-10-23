#!/usr/bin/env python3
"""Build a small sample library and push it into a (shut down) Mirage device.

    seed_library.py DEVICE [--tracks N] [--dry-run] [--keep DIR]

Each track gets an MP3 (a quiet tone at 128 kbit/s), a 176x176 cover BIN (a colour wash),
an LRC file (taken from ~/Music/*/*.lrc when there are any, a built-in sample otherwise) and a
palette picked from the cover colours -- the same layout the importer writes
(`library.json` version 2, `tracks/<id>/{audio.mp3,cover.bin,lyrics.lrc}`). `--dry-run` adds
the `dry-run` marker so the player runs without an audio sink, which is what Mirage needs.
Needs ffmpeg with libmp3lame.
"""
import argparse
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIRAGECTL = Path(os.environ.get("MIRAGECTL", "/Volumes/EXT0/Mirage/.build/out/Products/Debug/miragectl"))
DEVICE_ROOT = "/canopus/m.lyra_player"

SAMPLE_LRC = """[ti:Sample]
[00:00.00]
[00:04.20]Lyra Player sample lyrics
[00:07.80]第二行是中文的歌词，稍微长一点用来测试换行
[00:12.50]Third line, a bit longer to test the wrapping of text
[00:17.00]四行目は日本語の歌詞です
[00:21.50]Short
[00:24.00]一二三四五六七八九十一二三四五六七八九十
[00:29.00]And the final line of this tiny sample
[00:35.00]
"""

TRACKS = [
    ("ubique", "選り好み", ["ubique"], "選り好み", ((0xFF, 0x9E, 0xC4), (0x8F, 0xD3, 0xFF), (0xFF, 0xE2, 0x8A), (0xB8, 0x9B, 0xFF))),
    ("青山吉能 / 宮崎ヒヨリ", "となりあわせ", ["青山吉能", "宮崎ヒヨリ"], "となりあわせ", ((0x3E, 0xC9, 0xA7), (0xF7, 0xF1, 0xC8), (0x4D, 0x8B, 0xF5), (0xFF, 0xB7, 0x7B))),
    ("西沢さんP", "夜もすがら君想ふ (10th Anniv.)", ["西沢さんP", "GUMI"], "夜もすがら君想ふ", ((0x23, 0x1F, 0x5C), (0xE5, 0x4B, 0x8B), (0x6C, 0xA6, 0xF2), (0xF4, 0xC4, 0x6A))),
    ("Lyra", "A Rather Long Track Title That Needs The Marquee", ["Some Artist", "Another Artist", "Third"], "Samples", ((0xFF, 0x7A, 0x45), (0xFF, 0xC8, 0x57), (0x2B, 0x2D, 0x42), (0xEF, 0x56, 0x6B))),
]


def encoder():
    spec = importlib.util.spec_from_file_location("encode_lvgl_icon", ROOT / "scripts/encode-lvgl-icon.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *map(str, args)], check=True)


def hexcolor(rgb):
    return "0x%02X%02X%02X" % rgb


def mix(a, b, amount):
    return tuple(int(a[i] * (1 - amount) + b[i] * amount) for i in range(3))


def make_cover(colors, path_png):
    c = [hexcolor(rgb) for rgb in colors]
    ffmpeg("-f", "lavfi", "-i",
           f"gradients=s=176x176:c0={c[0]}:c1={c[1]}:c2={c[2]}:c3={c[3]}:nb_colors=4:x0=10:y0=0:x1=170:y1=176:d=1:speed=0.0001",
           "-frames:v", "1", path_png)


def lrc_files():
    found = []
    for base in (Path.home() / "Music").glob("*"):
        if base.is_dir():
            found += sorted(base.glob("*.lrc"))
    return found


def build(out, count):
    enc = encoder()
    lrcs = lrc_files()
    tracks = []
    for index in range(count):
        artist_line, name, artists, album, colors = TRACKS[index % len(TRACKS)]
        track_id = 1001 + index
        folder = out / "tracks" / str(track_id)
        folder.mkdir(parents=True, exist_ok=True)
        duration = int(os.environ.get("LYRA_SEED_SECONDS", "150")) + 6 * index
        ffmpeg("-f", "lavfi", "-i", f"sine=frequency={220 + 40 * index}:duration={duration}",
               "-af", "volume=0.15", "-c:a", "libmp3lame", "-b:a", "128k", "-ar", "44100", "-ac", "2",
               folder / "audio.mp3")
        png = folder / "cover.png"
        make_cover(colors, png)
        enc.convert(png, folder / "cover.bin")
        png.unlink()
        lrc = lrcs[index % len(lrcs)].read_text(errors="replace") if lrcs and index < len(lrcs) else SAMPLE_LRC
        (folder / "lyrics.lrc").write_text(lrc)
        tracks.append({
            "id": track_id, "name": name, "artists": artists, "album": album, "duration_ms": duration * 1000,
            "assets": 3,
            "palette": {"top": int.from_bytes(bytes(mix(colors[0], (0x10, 0x10, 0x18), 0.45)), "big"),
                        "bottom": int.from_bytes(bytes(mix(colors[2], (0x08, 0x08, 0x0C), 0.82)), "big")},
        })
    (out / "library.json").write_text(json.dumps({"version": 2, "revision": 1, "tracks": tracks}, ensure_ascii=False))


def ctl(device, *args):
    return subprocess.run([str(MIRAGECTL), *map(str, args)], capture_output=True, text=True)


def push(device, out, dry_run):
    ctl(device, "fs", "mkdir", device, "nand_data", DEVICE_ROOT)
    ctl(device, "fs", "mkdir", device, "nand_data", f"{DEVICE_ROOT}/tracks")
    files = sorted(p for p in out.rglob("*") if p.is_file())
    for path in files:
        relative = path.relative_to(out).as_posix()
        parent = Path(relative).parent
        if parent.as_posix() != ".":
            ctl(device, "fs", "mkdir", device, "nand_data", f"{DEVICE_ROOT}/{parent.as_posix()}")
        result = ctl(device, "fs", "put", device, "nand_data", path, f"{DEVICE_ROOT}/{relative}")
        if result.returncode != 0:
            sys.exit(f"put {relative}: {result.stderr or result.stdout}")
    if dry_run:
        marker = out / "dry-run"
        marker.write_text("1")
        result = ctl(device, "fs", "put", device, "nand_data", marker, f"{DEVICE_ROOT}/dry-run")
        if result.returncode != 0:
            sys.exit(result.stderr or result.stdout)


def main_for(device, tracks, dry_run=True):
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp)
        build(out, tracks)
        push(device, out, dry_run)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("device")
    ap.add_argument("--tracks", type=int, default=3)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--keep")
    args = ap.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(args.keep or tmp)
        out.mkdir(parents=True, exist_ok=True)
        build(out, args.tracks)
        push(args.device, out, args.dry_run)
    print("seeded", args.tracks, "tracks into", args.device)


if __name__ == "__main__":
    main()
