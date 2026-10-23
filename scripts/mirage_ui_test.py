#!/usr/bin/env python3
"""Drive Lyra Player's UI on a Mirage session and collect screenshots.

    mirage_ui_test.py 155 [--deploy] [--seed N] [--shots DIR] [SCENARIO ...]

Scenarios: library, player, lyrics, back, edge, mini, playlist (default: all but playlist, in
this order -- they build on each other: `library` leaves the app on the "all songs" page). `--deploy` rebuilds the module,
installs it and power-cycles the device first (about 90 s); `--seed N` re-seeds N sample tracks
during that restart. Emulator evidence only; the screenshots are for people to look at, the
asserts only check that the screen is lit and changes where it must.
"""
import argparse
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import mirage_session  # noqa: E402
from mirage_dev import Dev  # noqa: E402


class Layout:
    """Touch points, per panel."""

    def __init__(self, size):
        w, h = size
        self.w, self.h = w, h
        if (w, h) == (212, 520):
            self.home_row = lambda i: (w // 2, 98 + 72 * i)   # "all songs", then the playlists
            self.row = lambda i: (w // 2, 98 + 54 + 72 * i)   # song rows, below the two play buttons
            self.shuffle, self.loop = (54, 84), (158, 84)     # the play buttons above the songs
            self.mini, self.mini_toggle = (90, 470), (180, 470)  # the now-playing bar and its button
            self.icon = (106, 100)                            # Lyra's launcher icon after scrolling to the end
            self.back, self.mode = (34, 36), (176, 36)
            self.prev, self.play, self.next = (40, 420), (106, 420), (172, 420)
            self.vol_down, self.vol_up = (34, 483), (178, 483)
            self.quiet = (106, 24)                            # inert spot that keeps the screen on
            self.lyrics_swipe = (190, 300, 20, 300)
            self.player_swipe = (20, 300, 190, 300)
        else:
            self.home_row = lambda i: (w // 2, 104 + 80 * i)
            self.row = lambda i: (w // 2, 104 + 58 + 80 * i)
            self.shuffle, self.loop = (87, 88), (249, 88)
            self.mini, self.mini_toggle = (150, 430), (290, 430)
            self.icon = (40, 275)                             # clipped at the launcher's left edge
            self.back, self.mode = (42, 36), (294, 36)
            self.prev, self.play, self.next = (88, 424), (168, 424), (248, 424)
            self.vol_down, self.vol_up = (30, 424), (306, 424)
            self.quiet = (168, 24)
            self.lyrics_swipe = (310, 300, 30, 300)
            self.player_swipe = (30, 300, 310, 300)


    def open_songs(self, d, settle=2.5):
        """From the home page: into "all songs"."""
        d.tap(*self.home_row(0), settle)


def library(d, l):
    d.shot("home")
    l.open_songs(d)
    d.shot("songs")


def player(d, l):
    d.tap(*l.row(0), 3)
    d.shot("player")
    d.tap(*l.play, 1)
    d.shot("player-paused")
    d.tap(*l.play, 1)
    d.tap(*l.next, 2)
    d.shot("player-next")
    d.tap(*l.mode, 1)
    d.shot("player-mode")
    d.tap(*l.vol_down, 1)
    d.tap(*l.vol_down, 1)
    d.shot("player-volume")


def lyrics(d, l):
    d.swipe(*l.lyrics_swipe, 250, 1.5)
    d.shot("lyrics-a")
    d.hold(10, l.quiet)
    d.shot("lyrics-b")
    d.swipe(l.w // 2, 420, l.w // 2, 160, 300, 1.0)
    d.shot("lyrics-browse")
    d.hold(6, l.quiet)
    d.shot("lyrics-follow-again")
    d.swipe(*l.player_swipe, 250, 1.5)
    d.shot("back-to-player")


def edge(d, l):
    """The system's own back gesture: a swipe from the left edge."""
    d.swipe(4, l.h // 2, l.w - 20, l.h // 2, 300, 3)
    d.shot("home-after-edge-swipe")


def back(d, l):
    d.tap(*l.back, 3)
    d.shot("songs-after-back")


def playlist(d, l):
    """From the home page: the first playlist, then both play buttons, then back to the home page."""
    d.tap(*l.home_row(1), 2.5)
    d.shot("playlist")
    d.tap(*l.shuffle, 3)
    d.shot("playlist-shuffle")
    d.tap(*l.back, 2.5)
    d.shot("playlist-after-back")
    d.tap(*l.loop, 3)
    d.shot("playlist-loop")
    d.tap(*l.back, 2.5)
    d.swipe(4, l.h // 2, l.w - 20, l.h // 2, 300, 3)
    d.shot("home-after-playlist")


def mini(d, l):
    """The now-playing bar (run after `player`, `back` and `edge`: it shows on the home page too)."""
    d.tap(*l.mini_toggle, 1.5)
    d.shot("mini-paused")
    d.tap(*l.mini_toggle, 1.5)
    d.shot("mini-playing")
    d.tap(*l.mini, 3)
    d.shot("player-from-mini")


SCENARIOS = {"library": library, "player": player, "lyrics": lyrics, "back": back, "edge": edge, "mini": mini,
             "playlist": playlist}
DEFAULT = ["library", "player", "lyrics", "back", "edge", "mini"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("key")
    ap.add_argument("scenarios", nargs="*")
    ap.add_argument("--deploy", action="store_true")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--shots", default=os.environ.get("MIRAGE_SHOTS", "shots"))
    a = ap.parse_args()
    session = mirage_session.Session(a.key, a.shots)
    if a.deploy:
        session.cycle(seed=a.seed)
    d = session.dev or Dev(session.device, a.shots)
    layout = Layout(d.size)
    d.open_app(layout.icon)
    for name in a.scenarios or DEFAULT:
        print("scenario", name)
        SCENARIOS[name](d, layout)


if __name__ == "__main__":
    main()
