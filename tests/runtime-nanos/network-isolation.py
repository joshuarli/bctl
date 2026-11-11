#!/usr/bin/env python3
import pathlib
import platform
import socket
import sys

sys.dont_write_bytecode = True
here = pathlib.Path(__file__).resolve().parent

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    listener.settimeout(0.1)
    port = str(listener.getsockname()[1])
    if platform.system() == 'Darwin':
        from guest_fixture import run
        run('/tests/network-isolation.c', 'network_isolation', False, (port,))
    else:
        from guest_fixture_linux import run
        run(here / 'network-isolation.c', 'network_isolation', (port,))
    try:
        client, _ = listener.accept()
    except socket.timeout:
        pass
    else:
        client.close()
        raise SystemExit('guest reached the host loopback listener')
