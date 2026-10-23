#!/usr/bin/env python3
"""Screen recordings of Lyra Player on a Mirage device, for checking animations.

    mirage_record.py KEY OUT_DIR [lyrics|swap|library ...]

Mirage has no video capture, so screenshots are taken back to back (about 8 per second) and
ffmpeg stitches them with their real timing into OUT_DIR/<KEY>-<scenario>.mp4; the frames stay in
OUT_DIR/<KEY>-<scenario>/ (`t<ms>.png`, milliseconds since the recording started). One capture at
a time on purpose: parallel requests return out of order, which garbles exactly the motion this
is for.

The device must already run the module (`mirage_session.py KEY deploy --seed 4`). Emulator only:
the frame rate is the capture's, not the watch's.

    lyrics   open the second song, go to its lyrics and record two line changes
    swap     on the player page: next, wait, previous
    library  with a song playing: the mini player and the equalizer, then a drag down and back
    advance  the list playing on by itself under the open song list (not run by default: it needs
             short songs, `LYRA_SEED_SECONDS=8 mirage_session.py KEY restart --seed 4`)

Every scenario starts from the launcher and leaves the app on the song list.
"""
import argparse
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from mirage_dev import Dev, MIRAGECTL  # noqa: E402
from mirage_ui_test import Layout  # noqa: E402
import mirage_session  # noqa: E402


class Recorder:
    def __init__(self, device, frames, workers=1):
        self.device, self.frames, self.workers = device, Path(frames), workers
        self.frames.mkdir(parents=True, exist_ok=True)
        self.shots, self.lock, self.stop_flag = [], threading.Lock(), threading.Event()

    def _work(self, index):
        n = 0
        while not self.stop_flag.is_set():
            path = self.frames / f".w{index}-{n:04d}.png"
            begin = time.monotonic()
            subprocess.run([str(MIRAGECTL), "screenshot", self.device, str(path)], capture_output=True)
            end = time.monotonic()
            with self.lock:
                self.shots.append(((begin + end) / 2, path))
            n += 1

    def __enter__(self):
        self.t0 = time.monotonic()
        self.threads = [threading.Thread(target=self._work, args=(i,)) for i in range(self.workers)]
        for thread in self.threads:
            thread.start()
        return self

    def __exit__(self, *_):
        self.stop_flag.set()
        for thread in self.threads:
            thread.join()

    def encode(self, out):
        shots = sorted(s for s in self.shots if s[1].exists())
        listing = self.frames / "frames.txt"
        lines = []
        for (t, path), following in zip(shots, shots[1:] + [None]):
            named = self.frames / f"t{int((t - self.t0) * 1000):06d}.png"
            path.rename(named)
            duration = (following[0] - t) if following else 0.1
            lines += [f"file '{named.name}'", f"duration {duration:.3f}"]
        lines.append(f"file '{named.name}'")
        listing.write_text("\n".join(lines) + "\n")
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", str(listing),
                        "-vf", "scale=iw*2:ih*2:flags=neighbor,fps=30", "-pix_fmt", "yuv420p", str(out)], check=True)
        print(out, f"({len(shots)} frames)")


def lyrics(d, l, record):
    l.open_songs(d)
    d.tap(*l.row(1), 3)
    d.swipe(*l.lyrics_swipe, 250, 1.5)
    d.hold(4, l.quiet)
    with record() as r:
        d.hold(24, l.quiet, every=3.0)
    d.swipe(*l.player_swipe, 250, 1.5)
    d.tap(*l.back, 2)
    return r


def swap(d, l, record):
    l.open_songs(d)
    d.tap(*l.row(1), 3)
    with record() as r:
        time.sleep(0.8)
        d.tap(*l.next, 1.6)
        d.tap(*l.prev, 1.6)
    d.tap(*l.back, 2)
    return r


def library(d, l, record):
    l.open_songs(d)
    d.tap(*l.row(1), 3)
    d.tap(*l.back, 2)
    with record() as r:
        d.hold(3, l.quiet)
        d.swipe(l.w // 2, l.h - 140, l.w // 2, 160, 700, 1.2)
        d.swipe(l.w // 2, 160, l.w // 2, l.h - 140, 700, 1.2)
    return r


def advance(d, l, record):
    l.open_songs(d)
    d.tap(*l.row(0), 2)
    d.tap(*l.back, 1)
    with record() as r:
        d.hold(16, l.quiet, every=3.0)
    return r


SCENARIOS = {"lyrics": lyrics, "swap": swap, "library": library, "advance": advance}
DEFAULT = ["lyrics", "swap", "library"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("key")
    ap.add_argument("out")
    ap.add_argument("scenarios", nargs="*")
    a = ap.parse_args()
    out = Path(a.out)
    session = mirage_session.Session(a.key, out / f"{a.key}-steps")
    d = session.dev or Dev(session.device, out / f"{a.key}-steps")
    layout = Layout(d.size)
    for index, name in enumerate(a.scenarios or DEFAULT):
        if index:
            # Out of the app first: scrolling the launcher from inside it lands somewhere else, and
            # the icon tap then opens another app.
            d.home()
        d.open_app(layout.icon)
        recorder = SCENARIOS[name](d, layout, lambda: Recorder(session.device, out / f"{a.key}-{name}"))
        recorder.encode(out / f"{a.key}-{name}.mp4")


if __name__ == "__main__":
    main()
