#!/usr/bin/env python3
"""A long-lived Mirage device for developing Lyra Player: build, deploy, reboot, open the app.

    mirage_session.py KEY create    fresh device (KEY: 155, 179 = Band 11, 043 = Band 10 Pro): pair, install the
                                    production installer face
    mirage_session.py KEY cycle     package this build for the device's target (bumped version_code), install it
                                    over CPC2 while the Supervisor is loaded, power cycle, Run [--seed N]
    mirage_session.py KEY deploy    like cycle, but from any device state: power cycle, Run, install, power cycle
    mirage_session.py KEY restart   power cycle and Run again (module state is kept)
    mirage_session.py KEY enable    enable the module (new installs are disabled); takes effect at the next boot
    mirage_session.py KEY enable-ui the same by clicking through the Manager (positions measured on old layouts)
    mirage_session.py KEY launch    launcher -> end (screenshots land in --shots)

Reuses the Canopus emulator tooling (installer build, Facetory packager, AstroBox bridge).
LYRA_DEVICE_<KEY>=NAME points a KEY at another emulator device (a fresh one, say) with its own work directory.
The AstroBox bridge has to speak the Supervisor's CPC2 ABI (2.0 since Canopus' ABI 2.0 change), or every phone
request times out silently. Emulator evidence only.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
from mirage_dev import Dev, MIRAGECTL  # noqa: E402

CANOPUS = Path(os.environ.get("CANOPUS_ROOT", ROOT / "../Canopus")).resolve()
BRIDGE = Path(os.environ.get("MAB_SMOKE", "/Volumes/EXT0/Mirage/AstroBoxBridge/target/smoke"))
PACKAGER = Path(os.environ.get("FACETORY_PACKAGER",
                               "/Volumes/EXT0/GitHub/facetory/rust/target/debug/examples/package_lua_watchface"))
CLI = CANOPUS / "target/debug/canopus"
WORK = Path(os.environ.get("LYRA_DEV_WORK", ROOT / "build/mirage"))

PROD_INSTALLER = CANOPUS / "watchfaces/canopus-installer-prod/xiaomi-band-10-pro"

PROFILES = {
    "155": dict(device="lyra-dev-155", target="xiaomi-band-11-4.100.155", size=(212, 520),
                firmware="miwear.watch.q66tc_v4.100.155_full_f1c824fe",
                pair_confirm=(106, 478), run=(106, 383), pair_wake=(106, 260), wake=[(106, 20)],
                idle=(106, 300), run_taps=(15, 3.0),
                manager=dict(open=(106, 500, 106, 100), scroll=(25, (106, 450, 106, 80)), icon=(106, 75),
                             home_scroll=(3, (106, 450, 106, 100)), modules_row=(106, 210), first_module=(106, 115),
                             scroll_top=None, third_row=(106, 310), confirm=(106, 460), wait=5)),
    "179": dict(device="lyra-dev-179", target="xiaomi-band-11-4.100.179", size=(212, 520),
                firmware="miwear.watch.q66tc_v4.100.179_full_a93678a9",
                pair_confirm=(106, 478), run=(106, 383), pair_wake=(106, 260), wake=[(106, 20)],
                idle=(106, 300), run_taps=(15, 3.0),
                manager=dict(open=(106, 500, 106, 100), scroll=(25, (106, 450, 106, 80)), icon=(106, 75),
                             home_scroll=(3, (106, 450, 106, 100)), modules_row=(106, 210), first_module=(106, 115),
                             scroll_top=None, third_row=(106, 310), confirm=(106, 460), wait=5)),
    # Band 10 Pro: a quick double tap wakes the panel; the installer is the prebuilt production face.
    "043": dict(device="lyra-dev-043", target="xiaomi-band-10-pro-3.101.043", size=(336, 480),
                firmware="miwear.watch.p67tc_v3.101.043_full_a4ce8564",
                pair_confirm=(250, 435), run=(168, 168), pair_wake=(168, 240), wake=[(168, 240), (168, 240)],
                idle=(168, 440), run_taps=(16, 2.2),
                manager=dict(open=(168, 450, 168, 60), scroll=(12, (168, 440, 168, 60)), icon=(282, 288),
                             home_scroll=(2, (168, 420, 168, 120)), modules_row=(168, 155), first_module=(168, 100),
                             scroll_top=(2, (168, 150, 168, 440)), third_row=(168, 340), confirm=(168, 410), wait=3)),
}


def sh(*args, **kw):
    kw.setdefault("check", True)
    return subprocess.run([str(a) for a in args], **kw)


class Session:
    def __init__(self, key, shots):
        # LYRA_DEVICE_<key> points the same scripts at another emulator device (a fresh one, say)
        # without touching the long-lived default; it gets its own work directory.
        override = os.environ.get(f"LYRA_DEVICE_{key}")
        self.p = dict(PROFILES[key], device=override) if override else PROFILES[key]
        self.key = key
        self.device = self.p["device"]
        self.work = WORK / (override or key)
        self.work.mkdir(parents=True, exist_ok=True)
        self.dev = Dev(self.device, shots) if self.exists() else None
        self.shots = shots

    def ctl(self, *args, check=True):
        r = subprocess.run([str(MIRAGECTL), *map(str, args)], capture_output=True, text=True)
        if check and r.returncode != 0:
            raise RuntimeError(f"miragectl {args}: {r.stderr or r.stdout}")
        return r.stdout

    def exists(self):
        return self.device in subprocess.run([str(MIRAGECTL), "list"], capture_output=True, text=True).stdout

    # -- phone side ---------------------------------------------------------------------------
    def phone(self, *methods, state=None):
        info = self.ctl("info", self.device)
        addr = [l.split()[-1] for l in info.splitlines() if l.startswith("bt addr")][0]
        key = [l.split()[-1] for l in info.splitlines() if l.startswith("bound")][0]
        port = [l for l in self.ctl("list").splitlines() if self.device in l][0].split("BT :")[1].rstrip(")")
        state = state or self.work / "abstate"
        proc = subprocess.Popen([str(BRIDGE), port, key, str(state), *methods], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, env=dict(os.environ, MAB_BAND=addr))
        if not (Path(state) / "link-keys").exists():
            # The band declines a pairing request that is not confirmed within about 8 s.
            time.sleep(3)
            for point in self.p["wake"]:
                self.ctl("tap", self.device, *point, check=False)
            for _ in range(8):
                time.sleep(1.5)
                self.ctl("tap", self.device, *self.p["pair_confirm"], check=False)
        out, _ = proc.communicate(timeout=600)
        (self.work / f"bridge-{time.time_ns()}.log").write_text(out)
        if proc.returncode != 0 or '"ok":false' in out:
            raise RuntimeError("bridge failed:\n" + out[-3000:])
        return out

    def lyra(self, *command, timeout=1800):
        """Runs the importer driver (tools/lyra-mirage-import) against this device."""
        info = self.ctl("info", self.device)
        addr = [l.split()[-1] for l in info.splitlines() if l.startswith("bt addr")][0]
        key = [l.split()[-1] for l in info.splitlines() if l.startswith("bound")][0]
        port = [l for l in self.ctl("list").splitlines() if self.device in l][0].split("BT :")[1].rstrip(")")
        tool = ROOT / "target/debug/lyra-mirage-import"
        sh("cargo", "build", "-q", "-p", "lyra-mirage-import", cwd=ROOT)
        r = subprocess.run([str(tool), "--smoke", str(BRIDGE), "--port", port, "--key", key,
                            "--state", str(self.work / "abstate"), "--band", addr, *map(str, command)],
                           capture_output=True, text=True, timeout=timeout)
        print(r.stdout, end="")
        print(r.stderr[-1500:], end="", file=sys.stderr)
        return r.returncode == 0

    # -- installer ------------------------------------------------------------------------------
    def build_installer(self):
        face = self.work / "face"
        shutil.rmtree(face, ignore_errors=True)
        face.mkdir()
        if self.p["size"][0] == 336:
            names = ["main.lua", "manager_icon.bin", f"canopus_supervisor-{self.p['target']}.bin",
                     f"canopus_boot-{self.p['target']}.bin"]
            for name in names:
                shutil.copy(PROD_INSTALLER / name, face / name)
        else:
            out = self.work / "installer"
            shutil.rmtree(out, ignore_errors=True)
            out.mkdir(parents=True)
            sh("sh", CANOPUS / "scripts/build_canopus_supervisor.sh", cwd=CANOPUS, capture_output=True,
               env=dict(os.environ, CANOPUS_TARGET=self.p["target"], CANOPUS_BAND11_OUTPUT_DIR=str(out)))
            for f in [out / "main.lua", *out.glob("*.bin")]:
                shutil.copy(f, face / f.name)
        png(face / "preview.png", *self.p["size"])
        sh(PACKAGER, face, "--name", "Canopus", "--package-id", "000000000031", "--width", self.p["size"][0],
           "--height", self.p["size"][1], "-o", self.work / "installer.bin", capture_output=True)

    def create(self):
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        self.ctl("delete", self.device, check=False)
        shutil.rmtree(self.work / "abstate", ignore_errors=True)
        self.ctl("create", self.device, "--firmware", self.p["firmware"])
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(60)
        self.build_installer()
        self.phone('install={"path":"%s","kind":"watchface"}' % (self.work / "installer.bin"),
                   'setWatchface={"id":"000000000031"}', "watchfaces")
        time.sleep(8)
        self.dev = Dev(self.device, self.shots)
        self.run_installer()

    def run_installer(self, settle=30):
        """The installer face is the current watchface: wake, Run, wait for the Supervisor and the
        module restore (which registers the launcher entry)."""
        self.dev.wake()
        if self.p["size"][0] != 336:
            self.dev.tap(*self.p["pair_wake"], 3)
        self.dev.tap(*self.p["run"], 1)
        if self.p["size"][0] == 336:
            # The loaded Supervisor's timers stall while the screen is off: keep poking an inert spot.
            count, interval = self.p["run_taps"]
            for _ in range(count):
                time.sleep(interval)
                self.dev.tap(*self.p["idle"], 0.2)
            self.dev.wake()
        else:
            time.sleep(settle)

    # -- module ---------------------------------------------------------------------------------
    def package(self):
        """Builds and packages the module for this device's target only, with a version code that
        grows on every call so the Supervisor takes it as an update."""
        text = (ROOT / "Canopus.toml").read_text()
        counter = self.work / "version_code"
        code = int(counter.read_text()) + 1 if counter.exists() else 1000
        counter.write_text(str(code))
        text = re.sub(r"(?m)^version_code = \d+", f"version_code = {code}", text)
        text = re.sub(r"(?s)\[targets\]\ninclude = \[.*?\]", f'[targets]\ninclude = ["{self.p["target"]}"]', text)
        manifest = ROOT / f"Canopus.dev-{self.key}.toml"
        manifest.write_text(text)
        output = self.work / "lyra.canopus"
        try:
            sh(CLI, "build", "--manifest", manifest, cwd=ROOT, capture_output=True)
            sh(CLI, "package", "--manifest", manifest, "--output", output, cwd=ROOT, capture_output=True)
        finally:
            manifest.unlink(missing_ok=True)
        return output

    def install_module(self):
        """CPC2 install of the freshly packaged module; an update of a resident module answers
        REBOOT_REQUIRED, which the bridge reports as success."""
        package = self.package()
        out = self.phone('install={"path":"%s","kind":"canopus"}' % package, "canopusModules")
        print(out[-500:])

    def restart(self, seed=0, dry_run=True):
        """Power cycle (optionally re-seeding the library while the device is off), then Run."""
        self.ctl("shutdown", self.device)
        time.sleep(3)
        if seed:
            sys.path.insert(0, str(HERE))
            import seed_library
            seed_library.main_for(self.device, seed, dry_run)
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(22)
        self.dev = Dev(self.device, self.shots)
        self.run_installer()

    def cycle(self, seed=0):
        self.install_module()
        self.restart(seed)

    def deploy(self, seed=0):
        """From any state: power cycle, Run (the Supervisor must be loaded for a CPC2 install), install
        the current build, power cycle again with the library re-seeded and Run once more."""
        self.ctl("shutdown", self.device, check=False)
        time.sleep(3)
        self.ctl("boot", self.device, "--no-shell")
        time.sleep(48 if self.p["size"][0] == 336 else 30)
        self.dev = Dev(self.device, self.shots)
        self.run_installer()
        self.install_module()
        self.restart(seed)

    def enable(self):
        """A freshly installed module is disabled. Enable it with the CPC2 command the Manager's own
        "启用" row sends (the same call AstroBox makes); it takes effect at the next boot, so `restart`
        afterwards. The Manager walk below clicked rows by position and broke whenever the Manager's page
        layout changed (it once hit "移除" and removed the module)."""
        self.phone('canopusSetEnabled={"module":"lyra_player","enabled":true}')

    def enable_manager_walk(self):
        """Enables and activates the module by clicking through the Manager. Walk the Manager (launcher > Manager > modules > the
        module > enable > activate now); coordinates measured on Mirage (see Canopus scripts/emu/ui_pages.py).
        The screen sleeps a few seconds after the last touch, so every step is a touch. On Band 10 Pro the
        Manager icon is the last launcher entry only until Lyra's own icon exists."""
        m = self.p["manager"]
        d = self.dev
        d.wake()

        def swipe(gesture, settle=2.5):
            self.ctl("swipe", self.device, *gesture, 300)
            time.sleep(settle)

        def tap(point, settle=0.6):
            self.ctl("tap", self.device, *point)
            time.sleep(settle)

        def body():
            probe = self.work / "enable-probe.png"
            self.ctl("screenshot", self.device, probe)
            return probe.read_bytes()

        swipe(m["open"], 3)
        count, gesture = m["scroll"]
        for _ in range(count):
            swipe(gesture, 0.4)
        tap(m["icon"], 4)
        before = body()
        count, gesture = m["home_scroll"]
        for _ in range(40):
            swipe(gesture, 2.5)
            if body() != before:
                break
        for _ in range(count - 1):
            swipe(gesture, 2.5)
        tap(m["modules_row"], 3.5)
        tap(m["first_module"], 3.5)

        def to_top():
            if m["scroll_top"]:
                for _ in range(m["scroll_top"][0]):
                    swipe(m["scroll_top"][1], 2.5)

        to_top()
        for step, wait in (("enable", 3), ("activate", m["wait"])):
            tap(m["third_row"], 1.5)
            d.shot("dialog-" + step)
            tap(m["confirm"], wait)
            to_top()
        d.shot("module-running")

    def launcher(self):
        """Launcher -> scroll to the end -> the Lyra icon (one entry above the last)."""
        self.dev.launcher_to_end()
        self.dev.shot("launcher")


def png(path, w, h):
    import struct
    import zlib
    rows = b"".join(b"\0" + bytes([40, 90, 200]) * w for _ in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                           + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("key", choices=list(PROFILES))
    ap.add_argument("cmd", choices=["create", "cycle", "deploy", "restart", "launch", "package", "install", "enable",
                                    "enable-ui"])
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--shots", default=os.environ.get("MIRAGE_SHOTS", str(WORK / "shots")))
    a = ap.parse_args()
    s = Session(a.key, a.shots)
    if a.cmd in ("cycle", "deploy", "restart"):
        getattr(s, a.cmd)(seed=a.seed)
    else:
        getattr(s, {"launch": "launcher", "install": "install_module",
                    "enable-ui": "enable_manager_walk"}.get(a.cmd, a.cmd))()


if __name__ == "__main__":
    main()
