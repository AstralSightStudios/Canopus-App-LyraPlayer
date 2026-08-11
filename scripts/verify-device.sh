#!/bin/sh
set -eu
ELF=${1:?usage: verify-device.sh path/to/module.elf [max-loaded-size]}
MAX_SIZE=${2:-0}

LOADED=$(python3 - "$ELF" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
    raise SystemExit("expected little-endian ELF32")
e_shoff = struct.unpack_from("<I", data, 0x20)[0]
e_shentsize = struct.unpack_from("<H", data, 0x2e)[0]
e_shnum = struct.unpack_from("<H", data, 0x30)[0]
loaded = 0
for index in range(e_shnum):
    offset = e_shoff + index * e_shentsize
    flags = struct.unpack_from("<I", data, offset + 0x8)[0]
    size = struct.unpack_from("<I", data, offset + 0x14)[0]
    if flags & 0x2:
        loaded += size
print(loaded)
PY
)

if [ "$MAX_SIZE" -ne 0 ] && [ "$LOADED" -gt "$MAX_SIZE" ]; then
    echo "module exceeds target max_size: $LOADED > $MAX_SIZE" >&2
    exit 1
fi
NM=${NM:-nm}
if "$NM" -u "$ELF" | grep -q .; then
    echo "module has undefined imports:" >&2
    "$NM" -u "$ELF" >&2
    exit 1
fi
file "$ELF"
echo "verified module loaded size: $LOADED bytes"
