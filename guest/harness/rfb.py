#!/usr/bin/env python3
"""Prove a SmolVM guest framebuffer: foot pixels exist, VNC keys change them."""
import argparse
import hashlib
import select
import socket
import struct
import sys
import time

RAW_ENCODING = 0
DESKTOP_SIZE = -223


class VncError(RuntimeError):
    pass


class Rfb:
    def __init__(self, host, port, connect_timeout):
        deadline = time.monotonic() + connect_timeout
        while True:
            try:
                self.sock = socket.create_connection((host, port), timeout=5)
                break
            except OSError as error:
                if time.monotonic() >= deadline:
                    raise VncError(f"connect {host}:{port}: {error}")
                time.sleep(0.2)
        self.sock.settimeout(30)
        self.buffer = bytearray()
        self.width = 0
        self.height = 0
        self.bpp_bytes = 4
        self.big_endian = False
        self.red_max = self.green_max = self.blue_max = 255
        self.red_shift = 16
        self.green_shift = 8
        self.blue_shift = 0
        self.fb = bytearray()
        self.handshake()

    def _read_exact(self, count):
        while len(self.buffer) < count:
            data = self.sock.recv(65536)
            if not data:
                raise VncError("VNC connection closed during handshake")
            self.buffer += data
        out = bytes(self.buffer[:count])
        del self.buffer[:count]
        return out

    def handshake(self):
        version = self._read_exact(12)
        if not version.startswith(b"RFB "):
            raise VncError(f"not an RFB server: {version!r}")
        print(f"VNC_SERVER {version.decode().strip()}")
        self.sock.sendall(b"RFB 003.008\n")
        count = self._read_exact(1)[0]
        if count == 0:
            reason = self._read_exact(struct.unpack(">I", self._read_exact(4))[0])
            raise VncError(f"server refused connection: {reason!r}")
        types = self._read_exact(count)
        if 1 not in types:
            raise VncError(f"security type None unavailable: {list(types)}")
        self.sock.sendall(b"\x01")
        result = struct.unpack(">I", self._read_exact(4))[0]
        if result != 0:
            reason = self._read_exact(struct.unpack(">I", self._read_exact(4))[0])
            raise VncError(f"security handshake refused: {reason!r}")
        self.sock.sendall(b"\x01")
        self.width, self.height = struct.unpack(">HH", self._read_exact(4))
        pixel_format = self._read_exact(16)
        bpp, depth, big, true = struct.unpack(">BBBB", pixel_format[:4])
        self.red_max, self.green_max, self.blue_max = struct.unpack(">HHH", pixel_format[4:10])
        self.red_shift, self.green_shift, self.blue_shift = pixel_format[10], pixel_format[11], pixel_format[12]
        self.bpp_bytes = bpp // 8
        self.big_endian = bool(big)
        if not true or self.bpp_bytes < 3:
            raise VncError(f"unsupported pixel format bpp={bpp} depth={depth} true={true}")
        name_length = struct.unpack(">I", self._read_exact(4))[0]
        name = self._read_exact(name_length).decode(errors="replace")
        self.fb = bytearray(self.width * self.height * self.bpp_bytes)
        print(f"VNC_SIZE {self.width}x{self.height} bpp={bpp} depth={depth} "
              f"rshift={self.red_shift} gshift={self.green_shift} bshift={self.blue_shift}")
        print(f"VNC_NAME {name}")
        self.sock.sendall(struct.pack(">BBHi", 2, 0, 1, RAW_ENCODING))
        self.sock.settimeout(None)

    def request_update(self, incremental=1):
        self.sock.sendall(struct.pack(">BBHHHH", 3, incremental, 0, 0, self.width, self.height))

    def fill(self, wait):
        readable, _, _ = select.select([self.sock], [], [], wait)
        if not readable:
            return False
        data = self.sock.recv(1 << 20)
        if not data:
            raise VncError("VNC connection closed")
        self.buffer += data
        return True

    def parse_one(self):
        data = self.buffer
        if not data:
            return None
        kind = data[0]
        if kind == 0:
            if len(data) < 4:
                return None
            count = struct.unpack(">H", data[2:4])[0]
            offset = 4
            rects = []
            for _ in range(count):
                if len(data) < offset + 12:
                    return None
                x, y, width, height, encoding = struct.unpack(">HHHHi", data[offset:offset + 12])
                offset += 12
                if encoding == RAW_ENCODING:
                    size = width * height * self.bpp_bytes
                    if len(data) < offset + size:
                        return None
                    rects.append((x, y, width, height, bytes(data[offset:offset + size])))
                    offset += size
                elif encoding == DESKTOP_SIZE:
                    rects.append((0, 0, width, height, None))
                else:
                    raise VncError(f"unsupported encoding {encoding}")
            del data[:offset]
            return ("update", rects)
        if kind == 1:
            if len(data) < 6:
                return None
            count = struct.unpack(">H", data[4:6])[0]
            if len(data) < 6 + count * 6:
                return None
            del data[:6 + count * 6]
            return ("colourmap", None)
        if kind == 2:
            del data[:1]
            return ("bell", None)
        if kind == 3:
            if len(data) < 8:
                return None
            length = struct.unpack(">I", data[4:8])[0]
            if len(data) < 8 + length:
                return None
            del data[:8 + length]
            return ("cuttext", None)
        raise VncError(f"unknown server message {kind}")

    def apply(self, rects):
        for x, y, width, height, payload in rects:
            if payload is None:
                self.width, self.height = width, height
                self.fb = bytearray(width * height * self.bpp_bytes)
                continue
            if width == 0 or height == 0:
                continue
            if x + width > self.width or y + height > self.height:
                raise VncError(f"rect {x},{y} {width}x{height} outside {self.width}x{self.height}")
            row = width * self.bpp_bytes
            stride = self.width * self.bpp_bytes
            for line in range(height):
                source = line * row
                target = (y + line) * stride + x * self.bpp_bytes
                self.fb[target:target + row] = payload[source:source + row]

    def pump(self, wait):
        deadline = time.monotonic() + wait
        while True:
            message = self.parse_one()
            if message is not None:
                kind, body = message
                if kind == "update":
                    self.apply(body)
                continue
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            if not self.fill(min(remaining, 0.25)) and time.monotonic() >= deadline:
                return

    def poll_until(self, seconds, condition):
        deadline = time.monotonic() + seconds
        while True:
            self.request_update()
            self.pump(0.2)
            if condition():
                return True
            if time.monotonic() >= deadline:
                return False

    def _value(self, index):
        start = index * self.bpp_bytes
        return int.from_bytes(self.fb[start:start + self.bpp_bytes],
                              "big" if self.big_endian else "little")

    def _rgb(self, index):
        value = self._value(index)
        red = (value >> self.red_shift) & self.red_max
        green = (value >> self.green_shift) & self.green_max
        blue = (value >> self.blue_shift) & self.blue_max
        if self.red_max != 255:
            red = red * 255 // self.red_max
        if self.green_max != 255:
            green = green * 255 // self.green_max
        if self.blue_max != 255:
            blue = blue * 255 // self.blue_max
        return red, green, blue

    def nonblack_sample(self, step=16):
        sampled = nonblack = 0
        for index in range(0, self.width * self.height, step):
            red, green, blue = self._rgb(index)
            sampled += 1
            if red + green + blue > 24:
                nonblack += 1
        return nonblack / sampled if sampled else 0.0

    def stats(self):
        total = self.width * self.height
        nonblack = 0
        magenta = 0
        rgb_sum = 0
        for index in range(total):
            red, green, blue = self._rgb(index)
            rgb_sum += red + green + blue
            if red + green + blue > 24:
                nonblack += 1
            if red > 180 and blue > 180 and green < 120:
                magenta += 1
        return {
            "nonblack": nonblack / total if total else 0.0,
            "avg": rgb_sum / total if total else 0.0,
            "magenta": magenta / total if total else 0.0,
        }

    def changed_pixels(self, reference, threshold=8):
        changed = 0
        for index in range(self.width * self.height):
            if abs(self._value(index) - reference[index]) >= threshold:
                changed += 1
        return changed

    def reference(self):
        return [self._value(index) for index in range(self.width * self.height)]

    def write_ppm(self, path):
        rows = bytearray()
        for index in range(self.width * self.height):
            rows.extend(self._rgb(index))
        with open(path, "wb") as stream:
            stream.write(b"P6\n%d %d\n255\n" % (self.width, self.height))
            stream.write(rows)

    def key(self, keysym, down):
        self.sock.sendall(struct.pack(">BBxxI", 4, 1 if down else 0, keysym))
        time.sleep(0.03)

    def type_text(self, text):
        shift = 0xFFE1
        sent = 0
        for character in text:
            if character == "\n":
                self.key(0xFF0D, True)
                self.key(0xFF0D, False)
            elif "a" <= character <= "z" or "0" <= character <= "9" or character == " ":
                keysym = ord(character)
                self.key(keysym, True)
                self.key(keysym, False)
            elif "A" <= character <= "Z":
                keysym = ord(character.lower())
                self.key(shift, True)
                self.key(keysym, True)
                self.key(keysym, False)
                self.key(shift, False)
            else:
                raise VncError(f"no keysym mapping for {character!r}")
            sent += 1
        return sent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--text", default="laputa")
    parser.add_argument("--no-enter", action="store_true")
    parser.add_argument("--out-prefix")
    parser.add_argument("--connect-timeout", type=float, default=30)
    parser.add_argument("--ready-timeout", type=float, default=90)
    parser.add_argument("--change-timeout", type=float, default=20)
    parser.add_argument("--min-nonblack-frac", type=float, default=0.005)
    parser.add_argument("--min-changed-pixels", type=int, default=200)
    parser.add_argument("--min-magenta-delta", type=float, default=0.0002)
    args = parser.parse_args()

    try:
        rfb = Rfb(args.host, args.port, args.connect_timeout)
        ready = rfb.poll_until(args.ready_timeout,
                               lambda: rfb.nonblack_sample() >= args.min_nonblack_frac)
        before = rfb.stats()
        print(f"VNC_FIRST_FRAME nonblack_frac={before['nonblack']:.6f} "
              f"avg_rgb={before['avg']:.1f} magenta_frac={before['magenta']:.6f} "
              f"hash={hashlib.sha256(rfb.fb).hexdigest()[:16]}")
        if args.out_prefix:
            rfb.write_ppm(f"{args.out_prefix}-before.ppm")
        if not ready:
            raise VncError(f"no non-black foot pixels after {args.ready_timeout:.0f}s "
                           f"(nonblack_frac={before['nonblack']:.6f} < {args.min_nonblack_frac})")

        text = args.text + ("" if args.no_enter else "\n")
        reference = rfb.reference()
        sent = rfb.type_text(text)
        print(f"VNC_KEYS_SENT count={sent} text={args.text!r} enter={not args.no_enter}")

        def changed():
            return rfb.changed_pixels(reference) >= args.min_changed_pixels

        if not rfb.poll_until(args.change_timeout, changed):
            raise VncError(f"typed keys changed fewer than {args.min_changed_pixels} pixels "
                           f"within {args.change_timeout:.0f}s")
        after = rfb.stats()
        changed_count = rfb.changed_pixels(reference)
        magenta_delta = after["magenta"] - before["magenta"]
        print(f"VNC_KEY_CHANGE changed_pixels={changed_count} "
              f"magenta_frac={after['magenta']:.6f} magenta_delta={magenta_delta:.6f} "
              f"hash={hashlib.sha256(rfb.fb).hexdigest()[:16]}")
        if args.out_prefix:
            rfb.write_ppm(f"{args.out_prefix}-after.ppm")
        if magenta_delta < args.min_magenta_delta:
            raise VncError(f"laputa marker missing: magenta delta {magenta_delta:.6f} "
                           f"< {args.min_magenta_delta}")
    except (VncError, OSError, ValueError, struct.error) as error:
        print(f"VNC_FAIL: {error}", file=sys.stderr)
        return 1
    print("VNC_RFB_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
