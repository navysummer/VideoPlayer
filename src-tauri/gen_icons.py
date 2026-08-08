#!/usr/bin/env python3
"""Generate simple gradient PNG icons for the app bundle."""
import struct, zlib, os

def png(path, width, height, bg=(11, 11, 20), accent=(228, 199, 166)):
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            t = y / float(height)
            r = int(bg[0] + (accent[0] - bg[0]) * t * 0.55)
            g = int(bg[1] + (accent[1] - bg[1]) * t * 0.55)
            b = int(bg[2] + (accent[2] - bg[2]) * t * 0.55)
            # subtle radial glow
            cx, cy = width * 0.5, height * 0.42
            d = ((x - cx) ** 2 + (y - cy) ** 2) ** 0.5
            maxd = width * 0.6
            if d < maxd:
                f = 1.0 - d / maxd
                r = min(255, r + int(40 * f))
                g = min(255, g + int(24 * f))
                b = min(255, b + int(20 * f))
            raw.append(r); raw.append(g); raw.append(b); raw.append(255)
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        c += struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        return c
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    out = b"\x89PNG\r\n\x1a\n"
    out += chunk(b"IHDR", ihdr)
    out += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    out += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(out)

os.makedirs("icons", exist_ok=True)
png("icons/32x32.png", 32, 32)
png("icons/128x128.png", 128, 128)
png("icons/128x128@2x.png", 256, 256)
print("PNG icons written")