#!/usr/bin/env python3
import sys

sys.dont_write_bytecode = True
from guest_fixture import run

run('tests/mini_chromium/fixtures/vsock_reconnect.c', 'vsock_reconnect', False)
run('/tests/vsock-host-close.c', 'vsock_host_close', True)
