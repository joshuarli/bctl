#!/usr/bin/env python3
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
import time

root = pathlib.Path(__file__).resolve().parents[2]
runtime = root / '.work/runtime/nanos'
tree = runtime / 'hydrogen'
kernel = tree / 'nanos/output/platform/pc/bin/kernel.img'
mkfs = tree / 'nanos/output/tools/bin/mkfs'
tmp = root / '.work/tmp'
tmp.mkdir(parents=True, exist_ok=True)


def run(fixture, name, guest_args=()):
    bctl = os.environ['BCTL_BIN']
    with tempfile.TemporaryDirectory(prefix='nf-', dir=tmp) as directory:
        work = pathlib.Path(directory)
        binary = work / name
        subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-static', str(fixture), '-o', str(binary)], check=True)
        image = work / 'fixture.img'
        arguments = [item for arg in guest_args for item in ('--arg', arg)]
        subprocess.run(['sh', str(tree / 'dev/image.sh'), '-o', str(image)] + arguments + [str(binary)],
                       check=True, stdout=subprocess.DEVNULL)
        subprocess.run([str(mkfs), '-e', '-l', 'profile', '-s', '64m', str(image.with_suffix('.profile.img'))],
                       check=True, stdout=subprocess.DEVNULL)
        machine, sockets = work / 'machine', work / 's'
        machine.mkdir()
        sockets.mkdir()
        subprocess.run([bctl, 'nanos-vm', 'launch', '--dir', str(machine), '--socket-dir', str(sockets),
                        '--image', str(image), '--profile-dir', str(work / 'profile'), '--cdp-socket', str(sockets / 'cdp'),
                        '--memory-mib', '512', '--cpus', '1', '--qemu', shutil.which('qemu-system-x86_64'),
                        '--qemu-img', shutil.which('qemu-img'), '--kernel', str(kernel),
                        '--vsock-backend', str(runtime / 'bin/vhost-device-vsock')], check=True, timeout=60)
        console = machine / 'console.log'
        deadline = time.monotonic() + 60
        output = ''
        while time.monotonic() < deadline and 'MINI_CHROMIUM_TEST' not in output:
            time.sleep(0.2)
            output = console.read_text(errors='replace') if console.exists() else ''
        control = sockets / 'control.sock'
        if control.exists():
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(control))
                client.sendall(b'stop\n')
                client.recv(256)
        while control.exists() and time.monotonic() < deadline + 30:
            time.sleep(0.2)
        print(output)
        if f'MINI_CHROMIUM_TEST name={name} status=PASS ' not in output:
            raise SystemExit(f'{name} regression failed')
