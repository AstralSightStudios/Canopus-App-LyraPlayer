#!/bin/sh
# Cross-builds Lyra Player, verifies its ET_REL module, creates a CMI1 receipt
# with the Canopus-local development signer, and stages a one-shot watchface.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CANOPUS=${CANOPUS_ROOT:-"$ROOT/../Canopus"}
TARGET_ID=${CANOPUS_TARGET:-xiaomi-band-10-pro-3.101.030}
TARGET_PROFILE="$ROOT/targets/$TARGET_ID.env"
[ -f "$TARGET_PROFILE" ] || {
    echo "error: unsupported module target: $TARGET_ID" >&2
    exit 1
}
[ -f "$CANOPUS/targets/$TARGET_ID/target.toml" ] || {
    echo "error: Canopus target pack not found: $TARGET_ID" >&2
    exit 1
}
. "$TARGET_PROFILE"

OUT=${CANOPUS_BUILD_OUT:-"$ROOT/build/$TARGET_ID"}
WATCHFACE=${CANOPUS_WATCHFACE_OUT:-"$ROOT/watchfaces/lyra-player"}
CC=${CC:-clang}
TRIPLE=$RUST_TARGET_TRIPLE
TOKEN=lyra_player
KEY_SOURCE=${MODULE_INSTALL_KEY:-"$CANOPUS/.canopus-local/module-installer-ed25519.pem"}
CANOPUS_CLI=${CANOPUS_CLI:-"$CANOPUS/target/debug/canopus"}
ICON_SOURCE=${LYRA_PLAYER_ICON:-/Volumes/EXT0/lyra-player-icon.png}

TARGET_FIRMWARE_SHA256=$(python3 - "$CANOPUS/targets/$TARGET_ID/target.toml" "$TARGET_ID" <<'PY'
import pathlib, sys, tomllib
profile = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
if profile.get("target_id") != sys.argv[2]:
    raise SystemExit("target profile identity does not match CANOPUS_TARGET")
digest = profile.get("firmware_sha256", "")
if len(digest) != 64:
    raise SystemExit("target profile has no valid firmware_sha256")
bytes.fromhex(digest)
print(digest)
PY
)
LIFECYCLE=$(python3 - "$ROOT/Canopus.toml" <<'PY'
import pathlib, sys, tomllib
value = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())["module"]["lifecycle"]
classes = {
    "removable": 0,
    "resident-after-activation": 1,
    "always-resident": 2,
    "patch-reboot-required": 3,
}
if value not in classes:
    raise SystemExit(f"unsupported module lifecycle: {value}")
print(classes[value])
PY
)

[ -f "$KEY_SOURCE" ] || {
    echo "error: Canopus local module signer not found: $KEY_SOURCE" >&2
    exit 1
}
[ -x "$CANOPUS_CLI" ] || {
    echo "error: Canopus CLI not built: $CANOPUS_CLI" >&2
    echo "build it with: cargo build --manifest-path $CANOPUS/Cargo.toml" >&2
    exit 1
}
mkdir -p "$OUT" "$WATCHFACE"

NIGHTLY=${NIGHTLY_CARGO:-cargo +nightly}
LEAN_RUSTFLAGS="-C panic=abort -C target-cpu=$RUST_TARGET_CPU -Z unstable-options \
  -Z function-sections=no -C symbol-mangling-version=hashed \
  -Z location-detail=none -Z fmt-debug=none"

printf '%s\n' "[1/4] cross-build Rust staticlib"
RUSTFLAGS="$LEAN_RUSTFLAGS" $NIGHTLY build \
    --manifest-path "$ROOT/Cargo.toml" --release --target "$TRIPLE" \
    -p lyra-player-device --no-default-features \
    --features "$RUST_TARGET_FEATURE"

printf '%s\n' "[2/4] link and verify relocatable module"
"$CC" --target=arm-none-eabi -mcpu="$RUST_TARGET_CPU" -mthumb -mfloat-abi=soft \
    -ffreestanding -fno-common -fno-builtin -fno-stack-protector \
    -fno-unwind-tables -fno-asynchronous-unwind-tables \
    -fdata-sections -ffunction-sections -Os -Wall -Wextra -Werror \
    -c "$ROOT/crates/lyra-player-device/c_shim/canopus_ctor.c" \
    -o "$OUT/canopus_ctor.o"
RUSTLIB="$ROOT/target/$TRIPLE/release/liblyra_player_device.a"
PRELIM="$OUT/lyra-player.prelim.elf"
CANDIDATE="$OUT/lyra-player.opaque-layout.elf"
FINAL="$OUT/lyra-player.elf"
VERIFY_PRELIM="$OUT/opaque-verifier-prelim.txt"
VERIFY_LAYOUT="$OUT/opaque-verifier-layout.txt"
FIXUP_C="$OUT/opaque-fixups.c"
FIXUP_O="$OUT/opaque-fixups.o"
FIXUP_JSON="$OUT/opaque-fixups.json"

link_module() {
    output=$1
    shift
    ld.lld -r --gc-sections -u canopus_module_descriptor \
        "$OUT/canopus_ctor.o" "$@" "$RUSTLIB" -o "$output"
}
compile_fixups() {
    "$CC" --target=arm-none-eabi -mcpu="$RUST_TARGET_CPU" -mthumb -mfloat-abi=soft \
        -ffreestanding -fno-common -fno-builtin -fno-stack-protector \
        -fno-unwind-tables -fno-asynchronous-unwind-tables \
        -fdata-sections -ffunction-sections -Os -Wall -Wextra -Werror \
        -c "$FIXUP_C" -o "$FIXUP_O"
}

# Rust constants can coincidentally look like unapproved firmware pointers. The
# verifier must remain strict, so encode exactly the reported words in the ELF
# and restore them from the first constructor before Rust runs.
link_module "$PRELIM"
if "$CANOPUS_CLI" verify "$PRELIM" \
    --target "$TARGET_ID" --targets-dir "$CANOPUS/targets" \
    >"$VERIFY_PRELIM" 2>&1; then
    echo "error: preliminary link unexpectedly required no opaque-word encoding" >&2
    exit 1
fi
python3 "$ROOT/scripts/encode-opaque-words.py" generate \
    --verifier-output "$VERIFY_PRELIM" --output-c "$FIXUP_C" \
    --metadata "$FIXUP_JSON"
compile_fixups
link_module "$CANDIDATE" "$FIXUP_O"
if "$CANOPUS_CLI" verify "$CANDIDATE" \
    --target "$TARGET_ID" --targets-dir "$CANOPUS/targets" \
    >"$VERIFY_LAYOUT" 2>&1; then
    echo "error: opaque layout unexpectedly required no encoding" >&2
    exit 1
fi
python3 "$ROOT/scripts/encode-opaque-words.py" generate \
    --verifier-output "$VERIFY_LAYOUT" --output-c "$FIXUP_C" \
    --metadata "$FIXUP_JSON"
compile_fixups
link_module "$FINAL" "$FIXUP_O"
python3 "$ROOT/scripts/encode-opaque-words.py" patch \
    --elf "$FINAL" --metadata "$FIXUP_JSON"
OBJCOPY=${RUST_OBJCOPY:-$(command -v rust-objcopy || find "$HOME/.rustup" -name rust-objcopy 2>/dev/null | head -1)}
[ -n "$OBJCOPY" ] && [ -x "$OBJCOPY" ] || {
    echo "error: rust-objcopy is required to produce a bounded installer artifact" >&2
    exit 1
}
"$OBJCOPY" --remove-section=.llvmbc --strip-debug --strip-unneeded \
    "$FINAL" "$OUT/lyra-player.elf.strip"
mv "$OUT/lyra-player.elf.strip" "$FINAL"
"$CANOPUS_CLI" verify "$FINAL" \
    --target "$TARGET_ID" --targets-dir "$CANOPUS/targets"
"$ROOT/scripts/verify-device.sh" "$FINAL" "$MODULE_MAX_SIZE"

printf '%s\n' "[3/4] extract temporary local signer and sign CMI1 receipt"
# The framework-owned key is copied into a mode-0700 temporary directory solely
# for this signing invocation. It is removed by the trap and never enters the
# watchface or build output.
umask 077
SIGN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/lyra-sign.XXXXXX")
trap 'rm -rf "$SIGN_DIR"' EXIT HUP INT TERM
SIGN_KEY="$SIGN_DIR/module-installer-ed25519.pem"
cp "$KEY_SOURCE" "$SIGN_KEY"
chmod 600 "$SIGN_KEY"
python3 "$CANOPUS/scripts/build-module-installer-receipt.py" \
    --module "$OUT/lyra-player.elf" \
    --module-id "$TOKEN" \
    --version 2 \
    --lifecycle "$LIFECYCLE" \
    --target-id "$TARGET_ID" \
    --firmware-sha256 "$TARGET_FIRMWARE_SHA256" \
    --private-key "$SIGN_KEY" \
    --output "$OUT/receipt.bin"
python3 - "$OUT/receipt.bin" "$SIGN_DIR/receipt-prefix.bin" "$SIGN_DIR/receipt.sig" <<'PY'
import pathlib, sys
receipt = pathlib.Path(sys.argv[1]).read_bytes()
if len(receipt) != 256:
    raise SystemExit("signed receipt has the wrong size")
pathlib.Path(sys.argv[2]).write_bytes(receipt[:192])
pathlib.Path(sys.argv[3]).write_bytes(receipt[192:])
PY
openssl pkey -in "$SIGN_KEY" -pubout -out "$SIGN_DIR/signer-public.pem" >/dev/null 2>&1
openssl pkey -in "$SIGN_KEY" -pubout -outform DER \
    -out "$SIGN_DIR/signer-public.der" >/dev/null 2>&1
python3 - "$CANOPUS/manager/service/canopus_supervisor_platform.c" \
    "$SIGN_DIR/signer-public.der" <<'PY'
import pathlib, re, sys
source = pathlib.Path(sys.argv[1]).read_text()
match = re.search(
    r"s_installer_public_key\[32\]\s*=\s*\{(?P<body>.*?)\};",
    source,
    re.S,
)
if match is None:
    raise SystemExit("cannot extract supervisor installer public key")
trusted = bytes(int(value, 16) for value in re.findall(r"0x([0-9a-fA-F]{2})", match.group("body")))
derived_der = pathlib.Path(sys.argv[2]).read_bytes()
if len(trusted) != 32 or derived_der[-32:] != trusted:
    raise SystemExit("local signer does not match the Canopus supervisor trust key")
PY
openssl pkeyutl -verify -pubin -rawin \
    -inkey "$SIGN_DIR/signer-public.pem" \
    -in "$SIGN_DIR/receipt-prefix.bin" \
    -sigfile "$SIGN_DIR/receipt.sig" >/dev/null
rm -rf "$SIGN_DIR"
trap - EXIT HUP INT TERM

printf '%s\n' "[4/4] validate and stage installer watchface"
[ -f "$ICON_SOURCE" ] || {
    echo "error: Lyra app icon source not found: $ICON_SOURCE" >&2
    exit 1
}
python3 "$ROOT/scripts/encode-lvgl-icon.py" "$ICON_SOURCE" "$WATCHFACE/appicon_lyra.bin"
for asset in lyra-previous.bin lyra-play.bin lyra-pause.bin lyra-next.bin; do
    source="$ROOT/watchfaces/lyra-player/$asset"
    destination="$WATCHFACE/$asset"
    [ -f "$source" ] || {
        echo "error: missing player control asset: $source" >&2
        exit 1
    }
    if [ "$source" != "$destination" ]; then
        cp "$source" "$destination"
    fi
done
cp "$OUT/lyra-player.elf" "$WATCHFACE/module.bin"
cp "$OUT/receipt.bin" "$WATCHFACE/receipt.bin"
chmod 644 "$WATCHFACE/module.bin" "$WATCHFACE/receipt.bin" "$WATCHFACE/appicon_lyra.bin" "$WATCHFACE"/lyra-*.bin
if command -v luac >/dev/null 2>&1; then
    luac -p "$WATCHFACE/main.lua"
fi
python3 - "$WATCHFACE" "$TARGET_ID" "$TARGET_FIRMWARE_SHA256" "$LIFECYCLE" <<'PY'
import hashlib, pathlib, struct, sys
watchface = pathlib.Path(sys.argv[1])
module = (watchface / "module.bin").read_bytes()
receipt = (watchface / "receipt.bin").read_bytes()
icon = (watchface / "appicon_lyra.bin").read_bytes()
controls = {
    name: (watchface / name).read_bytes()
    for name in ("lyra-previous.bin", "lyra-play.bin", "lyra-pause.bin", "lyra-next.bin")
}
if not module.startswith(b"\x7fELF") or not 512 <= len(module) <= 393216:
    raise SystemExit(f"invalid staged module size: {len(module)}")
if len(icon) != 54768 or icon[:4] != b"\x19\x10\x00\x00" or icon[4:8] != b"\x75\x00\x75\x00" or icon[8:10] != b"\xd4\x01":
    raise SystemExit(f"invalid staged app icon: {len(icon)} bytes")
for name, control in controls.items():
    if len(control) != 16396 or control[:4] != b"\x19\x10\x00\x00" or control[4:8] != b"\x40\x00\x40\x00" or control[8:10] != b"\x00\x01":
        raise SystemExit(f"invalid staged control icon {name}: {len(control)} bytes")
if len(receipt) != 256 or receipt[:4] != b"CMI1":
    raise SystemExit("invalid CMI1 receipt")
magic, version, header, flags, lifecycle, module_version, artifact_size, reserved = struct.unpack("<8I", receipt[:32])
if magic != 0x31494D43 or version != 1 or header != 256:
    raise SystemExit("invalid receipt header")
if lifecycle != int(sys.argv[4]):
    raise SystemExit("receipt lifecycle mismatch")
if artifact_size != len(module):
    raise SystemExit("receipt artifact size mismatch")
if receipt[32:64].split(b"\0", 1)[0] != b"lyra_player":
    raise SystemExit("receipt module id mismatch")
if receipt[64:112].split(b"\0", 1)[0].decode("ascii") != sys.argv[2]:
    raise SystemExit("receipt target mismatch")
if receipt[112:144].hex() != sys.argv[3]:
    raise SystemExit("receipt firmware identity mismatch")
if receipt[144:176] != hashlib.sha256(module).digest():
    raise SystemExit("receipt artifact digest mismatch")
print(f"watchface staged: target={sys.argv[2]} module={len(module)}B receipt=256B")
PY
printf '%s\n' "watchfaces/lyra-player is ready to install"
