#!/usr/bin/env python3
"""Reimport audio.mp3, cover.bin and lyrics.lrc, then verify the device files after shutdown."""
import argparse
import hashlib
import json
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import mirage_session


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("key", choices=mirage_session.PROFILES)
    parser.add_argument("source", type=Path)
    parser.add_argument("--id", type=int, required=True, help="track ID to create or replace")
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--duration-ms", type=int, default=25000)
    parser.add_argument("--device")
    parser.add_argument("--session-work", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    if args.out.resolve() == args.source.resolve():
        parser.error("--out must differ from the source directory")
    files = ("audio.mp3", "cover.bin", "lyrics.lrc")
    expected = {name: (args.source / name).read_bytes() for name in files}
    args.out.mkdir(parents=True, exist_ok=True)
    if args.device:
        mirage_session.PROFILES[args.key] = dict(mirage_session.PROFILES[args.key], device=args.device)
    session = mirage_session.Session(args.key, args.out)
    if args.session_work:
        session.work = args.session_work
        session.work.mkdir(parents=True, exist_ok=True)
    assert session.lyra("hello"), "CanopusConnect HELLO failed"
    for index in range(args.repeats):
        print(f"import {index + 1}/{args.repeats}", flush=True)
        assert session.lyra(
            "import", "--id", str(args.id), "--audio", args.source / "audio.mp3",
            "--cover", args.source / "cover.bin", "--lyrics", args.source / "lyrics.lrc",
            "--name", "Transfer Verified", "--artist", "Binary Stream",
            "--duration-ms", str(args.duration_ms),
        ), "import failed"
    session.ctl("shutdown", session.device)
    root = "/canopus/m.lyra_player"
    report = {"key": args.key, "id": args.id, "repeats": args.repeats, "files": {}}
    try:
        for name in files:
            destination = args.out / name
            session.ctl("fs", "get", session.device, "nand_data", f"{root}/tracks/{args.id}/{name}", destination)
            received = destination.read_bytes()
            assert received == expected[name], f"device file differs: {name}"
            report["files"][name] = {"bytes": len(received), "sha256": hashlib.sha256(received).hexdigest()}
        library_path = args.out / "library.json"
        session.ctl("fs", "get", session.device, "nand_data", f"{root}/library.json", library_path)
        library = json.loads(library_path.read_text())
        matches = [track for track in library["tracks"] if track["id"] == args.id]
        assert len(matches) == 1, "reimport must preserve exactly one library entry"
        assert matches[0]["assets"] == 3, "cover and lyrics must remain available"
        assert matches[0]["duration_ms"] == args.duration_ms
        (args.out / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2), flush=True)
    finally:
        session.ctl("boot", session.device)
        time.sleep(48 if session.p["size"][0] == 336 else 30)
        session.run_installer()
    assert session.lyra("hello"), "CanopusConnect did not recover after reboot"
    assert session.lyra("list"), "persisted library could not be read after reboot"


if __name__ == "__main__":
    main()
