#!/usr/bin/env python3
import socket
import sys

sys.dont_write_bytecode = True
from guest_fixture import run

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    listener.settimeout(0.1)
    run('/tests/network-isolation.c', 'network_isolation', False,
        (str(listener.getsockname()[1]),))
    try:
        client, _ = listener.accept()
    except socket.timeout:
        pass
    else:
        client.close()
        raise SystemExit('guest reached the host loopback listener')
