#!/bin/sh
# Builds one production installer watchface containing exact payload pairs for
# Xiaomi Band 10 Pro firmware 3.101.030, 3.101.036, and 3.101.043.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CANOPUS=${CANOPUS_ROOT:-"$ROOT/../Canopus"}
WATCHFACE=${CANOPUS_WATCHFACE_OUT:-"$ROOT/watchfaces/lyra-player-prod"}
mkdir -p "$WATCHFACE"

set -- \
  xiaomi-band-10-pro-3.101.030 \
  xiaomi-band-10-pro-3.101.036 \
  xiaomi-band-10-pro-3.101.043
for TARGET_ID do
  rm -f "$WATCHFACE/lyra-player-$TARGET_ID.bin"
  rm -f "$WATCHFACE/lyra-player-$TARGET_ID.cmi.bin"
done
cp "$ROOT/watchfaces/lyra-player/appicon_lyra.bin" "$WATCHFACE/appicon_lyra.bin"
for asset in lyra-previous.bin lyra-play.bin lyra-pause.bin lyra-next.bin; do
  cp "$ROOT/watchfaces/lyra-player/$asset" "$WATCHFACE/$asset"
done

cargo fmt --manifest-path "$ROOT/Cargo.toml" --all -- --check
luac -p "$WATCHFACE/main.lua"

for TARGET_ID do
  OUT="$ROOT/build/lyra-player-prod/$TARGET_ID"
  STAGE="$OUT/watchface"
  rm -rf "$STAGE"
  mkdir -p "$STAGE"
  cp "$WATCHFACE/main.lua" "$STAGE/main.lua"
  CANOPUS_TARGET="$TARGET_ID" \
  CANOPUS_BUILD_OUT="$OUT" \
  CANOPUS_WATCHFACE_OUT="$STAGE" \
    "$ROOT/scripts/build-install-watchface.sh"
  cp "$STAGE/module.bin" "$WATCHFACE/lyra-player-$TARGET_ID.bin"
  cp "$STAGE/receipt.bin" "$WATCHFACE/lyra-player-$TARGET_ID.cmi.bin"
  rm -rf "$STAGE"
done

python3 - "$ROOT" "$CANOPUS" "$WATCHFACE" \
  xiaomi-band-10-pro-3.101.030 \
  xiaomi-band-10-pro-3.101.036 \
  xiaomi-band-10-pro-3.101.043 <<'PY'
import hashlib
import pathlib
import struct
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
canopus = pathlib.Path(sys.argv[2])
watchface = pathlib.Path(sys.argv[3])
targets = sys.argv[4:]
expected_files = set()
module_digests = set()

for target in targets:
    stem = watchface / f"lyra-player-{target}"
    module_path = pathlib.Path(str(stem) + ".bin")
    receipt_path = pathlib.Path(str(stem) + ".cmi.bin")
    expected_files.update((module_path.name, receipt_path.name))
    module = module_path.read_bytes()
    receipt = receipt_path.read_bytes()
    assert 512 <= len(module) <= 393216
    assert module[:7] == b"\x7fELF\x01\x01\x01"
    assert struct.unpack_from("<HH", module, 16) == (1, 40)
    assert len(receipt) == 256 and receipt[:4] == b"CMI1"
    magic, version, header, _flags, lifecycle, module_version, artifact_size, _reserved = struct.unpack(
        "<8I", receipt[:32]
    )
    assert magic == 0x31494D43 and version == 1 and header == 256
    assert lifecycle in range(4) and module_version == 2
    assert artifact_size == len(module), (target, artifact_size, len(module))
    module_id = receipt[32:64].split(b"\0", 1)[0]
    receipt_target = receipt[64:112].split(b"\0", 1)[0].decode("ascii")
    receipt_firmware = receipt[112:144].hex()
    profile = tomllib.loads((canopus / "targets" / target / "target.toml").read_text())
    assert profile["target_id"] == target
    assert module_id == b"lyra_player"
    assert receipt_target == target, (receipt_target, target)
    assert receipt_firmware == profile["firmware_sha256"]
    module_digest = hashlib.sha256(module).digest()
    assert receipt[144:176] == module_digest
    module_digests.add(module_digest)

assert len(module_digests) == len(targets), "target payloads must not be identical"
appicon = (watchface / "appicon_lyra.bin").read_bytes()
assert len(appicon) == 54768 and appicon[:4] == b"\x19\x10\0\0"
icon_width, icon_height, icon_stride, icon_reserved = struct.unpack_from("<4H", appicon, 4)
assert (icon_width, icon_height, icon_stride, icon_reserved) == (117, 117, 468, 0)
assert len(appicon) == 12 + icon_height * icon_stride
assert appicon == (root / "watchfaces" / "lyra-player" / "appicon_lyra.bin").read_bytes()
for name in ("lyra-previous.bin", "lyra-play.bin", "lyra-pause.bin", "lyra-next.bin"):
    control = (watchface / name).read_bytes()
    reference = (root / "watchfaces" / "lyra-player" / name).read_bytes()
    assert len(control) == 16396 and control[:4] == b"\x19\x10\0\0"
    assert struct.unpack_from("<4H", control, 4) == (64, 64, 256, 0)
    assert control == reference

actual_files = {path.name for path in watchface.glob("lyra-player-*.bin")}
assert actual_files == expected_files, (sorted(actual_files), sorted(expected_files))
assert not (watchface / "module.bin").exists()
assert not (watchface / "receipt.bin").exists()
assert not list(watchface.glob("lyra-player-*.cmi"))
print("production watchface staged OK: " + ", ".join(targets))
PY

echo "watchfaces/lyra-player-prod is ready to install"
