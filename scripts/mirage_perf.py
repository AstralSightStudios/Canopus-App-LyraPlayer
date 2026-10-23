#!/usr/bin/env python3
"""CPU load of the player UI on a Mirage device: resting versus dragging the lyric list.

    mirage_perf.py KEY

KEY is 155 / 179 (Band 11) or 043 (Band 10 Pro). Reads the Idle task's share from NSH `ps`, so the
device must be booted with its shell enabled (set `"shell" : true` in the device's device.json while it is
shut down, then `miragectl boot DEVICE`) and have the Supervisor loaded. Drag the stock launcher yourself to
compare: on 043 both come out at about 75 % idle. Emulator numbers only; the emulated CPU is not the watch's.
"""
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import mirage_session  # noqa: E402
from mirage_dev import MIRAGECTL  # noqa: E402
from mirage_ui_test import Layout  # noqa: E402


def main():
    key = sys.argv[1]
    session = mirage_session.Session(key, HERE.parent / "build/mirage" / key / "perf")
    d, device = session.dev, session.device
    layout = Layout(d.size)
    w, h = d.size

    def idle():
        out = subprocess.run([str(MIRAGECTL), "sh", device, "ps"], capture_output=True, text=True).stdout
        return next(float(line.split()[-2].rstrip("%")) for line in out.splitlines() if "Idle_Task" in line)

    d.open_app(layout.icon)
    d.tap(*layout.row(0), 3)
    d.swipe(*layout.lyrics_swipe, 250, 2.0)
    rest = []
    for _ in range(4):
        d.hold(3, layout.quiet)
        rest.append(idle())
    print("resting idle %:", rest)
    drag = []
    for rep in range(3):
        ys = (h - 120, 160) if rep % 2 == 0 else (160, h - 120)
        worker = threading.Thread(target=lambda ys=ys: subprocess.run(
            [str(MIRAGECTL), "swipe", device, str(w // 2), str(ys[0]), str(w // 2), str(ys[1]), "4000"],
            capture_output=True))
        worker.start()
        time.sleep(0.8)
        drag += [idle() for _ in range(3)]
        worker.join()
    print("dragging idle %:", drag, "mean", round(statistics.mean(drag), 1))


if __name__ == "__main__":
    main()
