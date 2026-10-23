#!/usr/bin/env python3
"""Capture the README screenshots from a Mirage device that already runs the module.

    mirage_screenshots.py KEY OUTDIR [--empty]

KEY is 155 / 179 (Band 11) or 043 (Band 10 Pro). Writes `<KEY>-launcher.png`, `-library.png`, `-player.png`,
`-volume.png`, `-lyrics.png`, `-lyrics-browse.png` and `-library-playing.png` into OUTDIR; with `--empty` it then
wipes the library on the powered-off device, boots it again and also writes `-empty.png`.

The device must be booted with the Supervisor loaded (`mirage_session.py KEY deploy --seed 4` leaves it so).
Emulator screenshots only.
"""
import argparse
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import mirage_session  # noqa: E402
from mirage_dev import MIRAGECTL  # noqa: E402
from mirage_ui_test import Layout  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("key")
    ap.add_argument("out")
    ap.add_argument("--empty", action="store_true")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    session = mirage_session.Session(a.key, out)
    d = session.dev
    layout = Layout(d.size)

    def shot(name):
        d.wake()
        d.screenshot(out / f"{a.key}-{name}.png")
        print(out / f"{a.key}-{name}.png")

    d.launcher_to_end()
    shot("launcher")
    d.tap(*layout.icon, 4)
    d.hold(4, layout.quiet)                 # the rows arrive over a few refresh ticks
    shot("home")
    layout.open_songs(d)
    d.hold(4, layout.quiet)
    shot("library")
    d.tap(*layout.row(1), 3)                # the second song has real lyrics
    d.hold(5, layout.quiet)
    shot("player")
    d.tap(*layout.vol_down, 1)
    d.tap(*layout.vol_down, 1)
    shot("volume")
    d.swipe(*layout.lyrics_swipe, 250, 1.5)
    d.hold(14, layout.quiet)                # let the highlight move onto a line
    shot("lyrics")
    d.swipe(d.size[0] // 2, d.size[1] - 120, d.size[0] // 2, 200, 400, 1.0)
    shot("lyrics-browse")
    d.swipe(*layout.player_swipe, 250, 1.5)
    d.tap(*layout.back, 3)
    d.hold(2, layout.quiet)
    shot("library-playing")
    if a.empty:
        subprocess.run([str(MIRAGECTL), "shutdown", session.device], check=True)
        time.sleep(3)
        subprocess.run([str(MIRAGECTL), "fs", "rm", session.device, "nand_data", "/canopus/m.lyra_player/library.json"],
                       check=False)
        subprocess.run([str(MIRAGECTL), "boot", session.device, "--no-shell"], check=True)
        time.sleep(48 if d.size[0] == 336 else 30)
        session.run_installer()
        d.launcher_to_end()
        d.tap(*layout.icon, 4)
        d.hold(3, layout.quiet)
        shot("empty")


if __name__ == "__main__":
    main()
