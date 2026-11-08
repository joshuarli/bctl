#!/usr/bin/env python3
"""Assert a screenshot is really rendered: non-trivial non-black fraction."""
import struct
import sys
import zlib


def read_png_rgb(path):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a png"
    pos = 8
    w = h = ctype = None
    raw = b""
    while pos < len(data):
        (ln,) = struct.unpack(">I", data[pos : pos + 4])
        typ = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + ln]
        if typ == b"IHDR":
            w, h, bd, ctype, comp, filt, inter = struct.unpack(">IIBBBBB", chunk)
            assert bd == 8 and inter == 0, f"unsupported png {bd} {inter}"
        elif typ == b"IDAT":
            raw += chunk
        pos += 12 + ln
    px = zlib.decompress(raw)
    ch = {0: 1, 2: 3, 6: 4}[ctype]
    stride = w * ch
    out = bytearray()

    def paeth(a, b, c):
        p = a + b - c
        pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
        if pa <= pb and pa <= pc:
            return a
        if pb <= pc:
            return b
        return c

    prev = bytearray(stride)
    p = 0
    for _ in range(h):
        f = px[p]
        p += 1
        line = bytearray(px[p : p + stride])
        p += stride
        if f == 1:  # Sub
            for i in range(ch, stride):
                line[i] = (line[i] + line[i - ch]) & 0xFF
        elif f == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:  # Average
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:  # Paeth
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                b = prev[i]
                c = prev[i - ch] if i >= ch else 0
                line[i] = (line[i] + paeth(a, b, c)) & 0xFF
        elif f != 0:
            raise AssertionError(f"unsupported png filter {f}")
        out += line
        prev = line
    return bytes(out), w, h, ch


def main():
    path, min_frac = sys.argv[1], float(sys.argv[2])
    px, w, h, ch = read_png_rgb(path)
    n = w * h
    magenta = 0
    nonblack = 0
    total_rgb = 0
    for i in range(n):
        r = px[i * ch]
        g = px[i * ch + 1] if ch > 1 else 0
        b = px[i * ch + 2] if ch > 1 else 0
        if r > 180 and b > 180 and g < 120:
            magenta += 1
        total_rgb += r + g + b
        if r + g + b > 24:
            nonblack += 1
    frac = nonblack / n
    print(f"SCREENSHOT {w}x{h} nonblack_frac={frac:.3f} avg_rgb={total_rgb / n:.1f} magenta_frac={magenta / n:.6f}")
    if frac < min_frac:
        print(f"FAIL: nonblack {frac:.3f} < {min_frac}")
        return 1
    if len(sys.argv) > 3 and magenta / n < float(sys.argv[3]):
        print("FAIL: missing magenta marker")
        return 1
    print("SCREENSHOT_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
