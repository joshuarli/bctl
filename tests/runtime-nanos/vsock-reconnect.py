#!/usr/bin/env python3
import pathlib
import socket
import subprocess
import tempfile
import threading

root = pathlib.Path(__file__).resolve().parents[2]
tree = root / '.work/runtime/nanos-arm64/hydrogen'
runner = root / '.work/runtime/nanos-krun/nanos-krun'
firmware = runner.with_name('firmware.fd')
tmp = root / '.work/tmp'
tmp.mkdir(parents=True, exist_ok=True)

with tempfile.TemporaryDirectory(prefix='vsock-', dir=tmp) as directory:
    work = pathlib.Path(directory)
    docker = ['docker', 'run', '--rm', '--platform', 'linux/arm64',
              '-v', f'{tree}:/src', '-v', f'{work}:/scratch', '-w', '/src',
              '-e', 'TMPDIR=/src/.work/tmp', 'bctl/nanos-arm64-builder']
    subprocess.run(docker + ['gcc', '-O2', '-Wall', '-Wextra', '-Werror', '-static',
                             'tests/mini_chromium/fixtures/vsock_reconnect.c',
                             '-o', '/scratch/fixture'], check=True)
    subprocess.run(docker + ['sh', 'dev/image.sh', '-o', '/scratch/disk.img',
                             '/scratch/fixture'], check=True, stdout=subprocess.DEVNULL)

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(work / 'egress'))
    server.listen(128)
    server.settimeout(0.1)
    stop = threading.Event()

    def echo(conn):
        with conn:
            try:
                data = conn.recv(1)
                if data:
                    conn.sendall(data)
                    while conn.recv(1024):
                        pass
            except OSError:
                pass

    def accept():
        while not stop.is_set():
            try:
                conn, _ = server.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=echo, args=(conn,), daemon=True).start()

    thread = threading.Thread(target=accept)
    thread.start()
    try:
        command = [str(runner), str(work / 'disk.img'), str(firmware),
                   str(work / 'cdp'), str(work / 'check'), str(work / 'egress'), '2', '1024']
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, timeout=90, check=True)
    finally:
        stop.set()
        thread.join()
        server.close()
    print(result.stdout)
    if 'MINI_CHROMIUM_TEST name=vsock_reconnect status=PASS ' not in result.stdout:
        raise SystemExit('vsock reconnect regression failed')
