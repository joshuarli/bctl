"""Run the vsock COW regression with a vhost-device-vsock backend."""

from pathlib import Path
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / '.work'


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def run(cpus):
    run = Path(tempfile.mkdtemp(prefix=f'vsock-cow-smp{cpus}-', dir=WORK / 'runs'))
    backend = os.environ['VHOST_DEVICE_VSOCK']
    processes = []
    try:
        with (run / 'backend.log').open('w') as log:
            processes.append(subprocess.Popen([backend, '--vm',
                f'guest-cid=3,socket={run}/vhost.sock,uds-path={run}/vsock.sock'], stdout=log, stderr=log))
        deadline = time.monotonic() + 5
        while not (run / 'vhost.sock').exists():
            if processes[0].poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError('vsock backend did not start')
            time.sleep(.01)
        with (run / 'qemu.log').open('w') as log:
            vm = subprocess.Popen(['qemu-system-x86_64', '-machine', 'q35,accel=kvm,memory-backend=mem',
                '-cpu', 'host', '-smp', str(cpus), '-m', '512M', '-object',
                'memory-backend-memfd,id=mem,size=512M,share=on', '-display', 'none', '-monitor', 'none',
                '-serial', f'file:{run}/serial.log', '-qmp', f'unix:{run}/qmp.sock,server=on,wait=off',
                '-no-reboot', '-device', 'isa-debug-exit',
                '-drive', f'if=none,id=hd0,format=raw,file={WORK}/images/vsock-cow.img',
                '-device', 'virtio-scsi-pci,id=scsi0', '-device', 'scsi-hd,bus=scsi0.0,drive=hd0',
                '-nic', 'none', '-kernel', str(ROOT / 'nanos/output/platform/pc/bin/kernel.img'),
                '-chardev', f'socket,id=vsock,path={run}/vhost.sock',
                '-device', 'vhost-user-vsock-pci,chardev=vsock'], stdout=log, stderr=log)
            processes.append(vm)
        deadline = time.monotonic() + 10
        serial = run / 'serial.log'
        while not serial.exists() or 'cow-ready' not in serial.read_text():
            if vm.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError('guest did not start')
            time.sleep(.01)
        for iteration in range(100):
            with socket.socket(socket.AF_UNIX) as conn:
                conn.settimeout(5)
                conn.connect(str(run / 'vsock.sock'))
                conn.sendall(b'CONNECT 9224\n')
                reply = b''
                while not reply.endswith(b'\n') and len(reply) < 64:
                    part = conn.recv(1)
                    if not part:
                        raise RuntimeError('handshake EOF')
                    reply += part
                if not reply.startswith(b'OK '):
                    raise RuntimeError(f'bad handshake: {reply!r}')
                for chunk in range(4):
                    conn.sendall(bytes([chunk + 1]) * 65536)
                reply = b''
                while len(reply) < 5:
                    part = conn.recv(5 - len(reply))
                    if not part:
                        raise RuntimeError(f'round {iteration}: reply EOF')
                    reply += part
                if reply != b'done\n':
                    raise RuntimeError(f'round {iteration}: {reply!r}')
        if vm.wait(timeout=10) != 1 or 'MINI_CHROMIUM_RESULT status=PASS passed=1 failed=0' not in serial.read_text():
            raise RuntimeError('guest validation failed')
        print(f'PASS vsock-cow smp={cpus} rounds=100 dir={run}', flush=True)
    finally:
        for process in reversed(processes):
            stop(process)


def interrupted(signum, frame):
    raise SystemExit(128 + signum)


if __name__ == '__main__':
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    for cpus in (map(int, sys.argv[1:]) if sys.argv[1:] else (1, 2, 4)):
        run(cpus)
