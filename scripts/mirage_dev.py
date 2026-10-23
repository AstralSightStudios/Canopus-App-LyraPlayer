#!/usr/bin/env python3
"""Drive a Mirage device for UI work: wake, touch, swipe, screenshots and launcher navigation.

Usage as a module (`from mirage_dev import Dev`) or as a CLI:

    mirage_dev.py DEVICE shot NAME            screenshot into --shots/NAME.png
    mirage_dev.py DEVICE tap X Y [SHOT]
    mirage_dev.py DEVICE swipe X0 Y0 X1 Y1 [MS] [SHOT]
    mirage_dev.py DEVICE open-app NAME_ICON_Y  launcher: scroll to the end and tap the app icon

Mirage switches the screen off a few seconds after the last touch; every helper wakes it first.
"""
import hashlib
import os
import subprocess
import sys
import time
from pathlib import Path

MIRAGECTL = Path(os.environ.get("MIRAGECTL", "/Volumes/EXT0/Mirage/.build/out/Products/Debug/miragectl"))
DARK_BYTES = 3000          # a dark screenshot compresses to ~2 KB


class Dev:
    def __init__(self, name, shots):
        self.name = name
        self.shots = Path(shots)
        self.shots.mkdir(parents=True, exist_ok=True)
        info = self.ctl("info")
        self.size = (212, 520) if "212x520" in info or "q66" in info else (336, 480)
        self.n = 0

    def ctl(self, *args, check=True):
        r = subprocess.run([str(MIRAGECTL), args[0], self.name, *map(str, args[1:])],
                           capture_output=True, text=True, timeout=240)
        if check and r.returncode != 0:
            raise RuntimeError(f"miragectl {args}: {r.stderr or r.stdout}")
        return r.stdout

    def raw(self, *args, check=True):
        r = subprocess.run([str(MIRAGECTL), *map(str, args)], capture_output=True, text=True, timeout=240)
        if check and r.returncode != 0:
            raise RuntimeError(f"miragectl {args}: {r.stderr or r.stdout}")
        return r.stdout

    def screenshot(self, path):
        self.ctl("screenshot", path)
        return Path(path)

    def lit(self):
        probe = self.shots / ".probe.png"
        self.screenshot(probe)
        return probe.stat().st_size > DARK_BYTES

    def wake(self):
        if not self.lit():
            # Band 10 Pro wakes on a quick double tap (and acts on the UI when already lit).
            for _ in range(2 if self.size[0] == 336 else 1):
                self.ctl("tap", self.size[0] // 2, 240 if self.size[0] == 336 else 20)
            time.sleep(1.2)

    def tap(self, x, y, settle=0.8):
        self.wake()
        self.ctl("tap", x, y)
        time.sleep(settle)

    def swipe(self, x0, y0, x1, y1, ms=300, settle=0.8):
        self.wake()
        self.ctl("swipe", x0, y0, x1, y1, ms)
        time.sleep(settle)

    def shot(self, name):
        self.wake()
        self.n += 1
        path = self.shots / f"{self.n:02d}-{name}.png"
        self.screenshot(path)
        print(path)
        return path

    def hold(self, seconds, point=None, every=4.0):
        """Waits while tapping an inert spot so the screen does not time out (the firmware drops
        back to the watchface a few seconds after the last touch)."""
        point = point or (self.size[0] // 2, 24)
        end = time.time() + seconds
        while time.time() < end:
            self.ctl("tap", *point)
            time.sleep(min(every, max(0.1, end - time.time())))

    def digest(self):
        probe = self.shots / ".digest.png"
        self.screenshot(probe)
        return hashlib.md5(probe.read_bytes()).hexdigest()

    def home(self):
        self.ctl("button", "home")
        time.sleep(1.5)

    def open_app(self, icon=(106, 100)):
        """Launcher -> last page -> the app icon."""
        self.launcher_to_end()
        self.shot("launcher")
        self.tap(*icon, 3)

    def launcher_to_end(self, limit=40):
        w, h = self.size
        self.wake()
        self.swipe(w // 2, h - 20, w // 2, 100, 300, 2.0)
        last = None
        for _ in range(limit):
            self.swipe(w // 2, h - 70, w // 2, 80, 300, 0.5)
            now = self.digest()
            if now == last:
                return
            last = now


def main():
    import argparse
    p = argparse.ArgumentParser()
    p.add_argument("device")
    p.add_argument("--shots", default=os.environ.get("MIRAGE_SHOTS", "shots"))
    p.add_argument("cmd")
    p.add_argument("args", nargs="*")
    a = p.parse_args()
    d = Dev(a.device, a.shots)
    if a.cmd == "shot":
        d.shot(a.args[0] if a.args else "shot")
    elif a.cmd == "tap":
        d.tap(int(a.args[0]), int(a.args[1]))
        if len(a.args) > 2:
            d.shot(a.args[2])
    elif a.cmd == "swipe":
        x0, y0, x1, y1 = map(int, a.args[:4])
        ms = int(a.args[4]) if len(a.args) > 4 and a.args[4].isdigit() else 300
        d.swipe(x0, y0, x1, y1, ms)
        if len(a.args) > 4 and not a.args[-1].isdigit():
            d.shot(a.args[-1])
    elif a.cmd == "launcher-end":
        d.launcher_to_end()
        d.shot("launcher-end")
    else:
        sys.exit("unknown command")


if __name__ == "__main__":
    main()
