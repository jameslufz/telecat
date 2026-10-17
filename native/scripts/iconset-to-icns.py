#!/usr/bin/env python3
"""Pack resized PNGs into an ICNS family without the IconServices/XPC encoder.

Format reference (resource tags, not a copied implementation):
https://github.com/python-pillow/Pillow/blob/main/src/PIL/IcnsImagePlugin.py
"""
import pathlib
import struct
import sys

source, destination = map(pathlib.Path, sys.argv[1:])
resources = [
    (b"icp4", 16, 1), (b"ic11", 16, 2),
    (b"icp5", 32, 1), (b"ic12", 32, 2),
    (b"ic07", 128, 1), (b"ic13", 128, 2),
    (b"ic08", 256, 1), (b"ic14", 256, 2),
    (b"ic09", 512, 1), (b"ic10", 512, 2),
]
headers, payloads = [], []
for tag, points, scale in resources:
    suffix = "@2x" if scale == 2 else ""
    data = (source / f"icon_{points}x{points}{suffix}.png").read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    assert struct.unpack(">II", data[16:24]) == (points * scale,) * 2
    header = struct.pack(">4sI", tag, 8 + len(data))
    headers.append(header)
    payloads.append(header + data)
toc = b"".join(headers)
body = struct.pack(">4sI", b"TOC ", 8 + len(toc)) + toc + b"".join(payloads)
destination.write_bytes(struct.pack(">4sI", b"icns", 8 + len(body)) + body)
