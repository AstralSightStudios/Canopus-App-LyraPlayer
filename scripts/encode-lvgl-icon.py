#!/usr/bin/env python3
"""Convert an 8-bit RGB/RGBA PNG to an LVGL v9 ARGB8888 BIN."""
from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def paeth(a: int, b: int, c: int) -> int:
    estimate = a + b - c
    pa = abs(estimate - a)
    pb = abs(estimate - b)
    pc = abs(estimate - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def decode_png(data: bytes) -> tuple[int, int, bytes]:
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("not a PNG file")
    offset = len(PNG_SIGNATURE)
    width = height = bit_depth = color_type = interlace = None
    compressed = bytearray()
    while offset + 12 <= len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        offset += 4
        kind = data[offset : offset + 4]
        offset += 4
        chunk = data[offset : offset + length]
        offset += length
        if offset + 4 > len(data):
            raise ValueError("truncated PNG chunk")
        expected_crc = struct.unpack_from(">I", data, offset)[0]
        offset += 4
        actual_crc = zlib.crc32(kind + chunk) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise ValueError(f"invalid PNG CRC for {kind!r}")
        if kind == b"IHDR":
            if len(chunk) != 13:
                raise ValueError("invalid PNG IHDR")
            width, height, bit_depth, color_type, compression, filtering, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if compression != 0 or filtering != 0:
                raise ValueError("unsupported PNG compression or filtering method")
        elif kind == b"IDAT":
            compressed.extend(chunk)
        elif kind == b"IEND":
            break
    if width is None or height is None:
        raise ValueError("PNG has no IHDR")
    if bit_depth != 8 or color_type not in (2, 6) or interlace != 0:
        raise ValueError("PNG must be non-interlaced 8-bit RGB or RGBA")
    channels = 4 if color_type == 6 else 3
    stride = width * channels
    raw = zlib.decompress(bytes(compressed))
    expected = height * (stride + 1)
    if len(raw) != expected:
        raise ValueError("PNG decompressed data has an unexpected length")
    rows: list[bytes] = []
    cursor = 0
    previous = bytes(stride)
    for _ in range(height):
        filter_type = raw[cursor]
        cursor += 1
        encoded = raw[cursor : cursor + stride]
        cursor += stride
        row = bytearray(stride)
        for index, value in enumerate(encoded):
            left = row[index - channels] if index >= channels else 0
            up = previous[index]
            upper_left = previous[index - channels] if index >= channels else 0
            if filter_type == 0:
                reconstructed = value
            elif filter_type == 1:
                reconstructed = (value + left) & 0xFF
            elif filter_type == 2:
                reconstructed = (value + up) & 0xFF
            elif filter_type == 3:
                reconstructed = (value + ((left + up) // 2)) & 0xFF
            elif filter_type == 4:
                reconstructed = (value + paeth(left, up, upper_left)) & 0xFF
            else:
                raise ValueError(f"unsupported PNG filter {filter_type}")
            row[index] = reconstructed
        rows.append(bytes(row))
        previous = bytes(row)
    rgba = bytearray()
    for row in rows:
        for index in range(0, stride, channels):
            rgba.extend(row[index : index + 3])
            rgba.append(row[index + 3] if channels == 4 else 255)
    return width, height, bytes(rgba)


def encode_lvgl_v9(width: int, height: int, rgba: bytes) -> bytes:
    if not 1 <= width <= 0xFFFF or not 1 <= height <= 0xFFFF:
        raise ValueError("image dimensions do not fit LVGL v9 header")
    if len(rgba) != width * height * 4:
        raise ValueError("RGBA pixel length does not match dimensions")
    pixels = bytearray()
    for index in range(0, len(rgba), 4):
        red, green, blue, alpha = rgba[index : index + 4]
        pixels.extend((blue, green, red, alpha))
    return struct.pack("<4sHHHH", b"\x19\x10\x00\x00", width, height, width * 4, 0) + pixels


def convert(source: Path, destination: Path) -> None:
    width, height, rgba = decode_png(source.read_bytes())
    encoded = encode_lvgl_v9(width, height, rgba)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(encoded)
    expected = 12 + width * height * 4
    if len(encoded) != expected:
        raise ValueError("encoded LVGL BIN has an unexpected length")


def self_test() -> None:
    # A filter-free 1x1 RGBA PNG with RGB=(0x11,0x22,0x33), A=0x44.
    scanline = b"\x00\x11\x22\x33\x44"
    def chunk(kind: bytes, payload: bytes) -> bytes:
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    png = PNG_SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(scanline)) + chunk(b"IEND", b"")
    width, height, rgba = decode_png(png)
    expected = bytes.fromhex("19 10 00 00 01 00 01 00 04 00 00 00 33 22 11 44")
    assert encode_lvgl_v9(width, height, rgba) == expected


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", nargs="?")
    parser.add_argument("destination", nargs="?")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if not args.source or not args.destination:
        parser.error("source and destination are required unless --self-test is used")
    try:
        convert(Path(args.source), Path(args.destination))
    except (OSError, ValueError, zlib.error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
