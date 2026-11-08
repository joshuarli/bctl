#!/usr/bin/env python3
"""Minimal QMP client over stdlib: capabilities, screendump, powerdown, quit."""
import json
import socket
import sys


def main():
    sock_path, command = sys.argv[1], sys.argv[2]
    arg = sys.argv[3] if len(sys.argv) > 3 else ""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(20)
    s.connect(sock_path)
    f = s.makefile("rwb")

    def recv():
        while True:
            line = f.readline()
            if not line:
                raise RuntimeError("qmp eof")
            msg = json.loads(line)
            if "event" in msg and "return" not in msg:
                continue
            return msg

    def send(cmd, args=None):
        payload = {"execute": cmd}
        if args:
            payload["arguments"] = args
        f.write((json.dumps(payload) + "\n").encode())
        f.flush()
        return recv()

    greeting = recv()
    if "QMP" not in greeting:
        raise RuntimeError(f"no qmp greeting: {greeting}")
    r = send("qmp_capabilities")
    if "error" in r:
        raise RuntimeError(f"capabilities failed: {r}")

    if command == "screendump":
        r = send("screendump", {"filename": arg, "format": "png"})
    elif command == "powerdown":
        r = send("system_powerdown")
    elif command == "quit":
        r = send("quit")
    elif command == "input":
        events = [{"type": "key", "data": {"key": {"type": "qcode", "data": code}, "down": down}}
                  for code in ["l", "a", "p", "u", "t", "a", "ret"] for down in [True, False]]
        try:
            r = send("input-send-event", {"events": events})
        except (OSError, EOFError, RuntimeError, ValueError) as error:
            print(f"QMP input outcome indeterminate: {error}; do not resend", file=sys.stderr)
            return 75
        if "error" in r:
            raise RuntimeError(f"input rejected: {r}")
    else:
        raise RuntimeError(f"unknown command {command}")
    if "error" in r:
        raise RuntimeError(f"{command} failed: {r}")
    print(f"QMP_{command.upper()}_OK")


if __name__ == "__main__":
    sys.exit(main())
