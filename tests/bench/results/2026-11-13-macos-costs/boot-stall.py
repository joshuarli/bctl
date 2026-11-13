"""Bounded boot-stall diagnostic; retains every VM disk and console."""

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path.cwd().resolve()
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m


def report(address):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(address))
        result = b''
        while True:
            chunk = stream.recv(4096)
            if not chunk:
                return result.decode()
            result += chunk
            if len(result) > 4096:
                raise RuntimeError('overlong guest report')


def control(address, command):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(address))
        stream.sendall(command.encode() + b'\n')
        return stream.recv(4096).decode()


def stop(process):
    m.stop(process)
    def members():
        table = m.invoke(['ps', '-A', '-o', 'pid=,pgid=,stat=,comm='])
        return [line for line in table.splitlines()
            if int(line.split(None, 3)[1]) == process.pid and not line.split(None, 3)[2].startswith('Z')]
    if not members():
        return
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGKILL)
    until = time.monotonic() + 3
    while time.monotonic() < until:
        if not members():
            return
        time.sleep(0.05)
    raise RuntimeError(f'owned VM group still has live processes: {members()}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=m.NANOS_RUNTIME)
    parser.add_argument('--rounds', type=int, default=10)
    parser.add_argument('--cpus', type=int, default=4)
    parser.add_argument('--deadline', type=float, default=10)
    parser.add_argument('--registers', action='store_true', help='use the diagnostic runtime to capture paused guest registers')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if ROOT != m.ROOT or args.rounds < 1 or not 1 <= args.cpus <= 16 or args.deadline <= 0:
        parser.error('run from bctl with positive rounds/deadline and 1–16 CPUs')
    home = Path(tempfile.mkdtemp(prefix='bs-', dir=m.SCRATCH))
    runtime = args.runtime.resolve()
    image = args.image.resolve()
    inputs = [Path(__file__).resolve(), Path(m.__file__), m.BCTL, image, image.with_suffix('.profile.img'),
        *[runtime / name for name in ('nanos-krun', 'lib/libkrun.dylib', 'firmware.fd')]]
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    receipt = {'completed': False, 'method': 'diagnostic only; explicit supervisor resource arguments; fresh root and profile disks; one two-CPU control followed by bounded candidate boots; stop at first readiness failure; no release timing claims',
        'configuration': vars(args) | {'image': str(image), 'runtime': str(runtime), 'out': str(args.out)},
        'inputs_sha256': {str(path): digest(path) for path in inputs}, 'attempts': []}
    env = os.environ | {'BCTL_STARTUP_TRACE': '1', 'LIBKRUN_TIMING': '1'}
    try:
        for index, cpus in enumerate([2] + [args.cpus] * args.rounds):
            root = home / str(index)
            for name in ('m', 's', 'p', 'f'):
                (root / name).mkdir(parents=True)
            command = [m.BCTL, 'nanos-vm', 'serve', '--dir', root / 'm', '--socket-dir', root / 's',
                '--image', image, '--cdp-socket', root / 'c', '--memory-mib', '4096', '--cpus', str(cpus),
                '--profile-dir', root / 'p', '--files-dir', root / 'f', '--runner', runtime / 'nanos-krun',
                '--firmware', runtime / 'firmware.fd', '--allow-private', '127.0.0.1/32']
            attempt = {'round': index, 'cpus': cpus, 'artifacts': str(root.relative_to(ROOT)), 'command': list(map(str, command))}
            receipt['attempts'].append(attempt)
            trial_env = env.copy()
            if args.registers:
                trial_env['HYDROGEN_BOOT_WATCH_CONTROL'] = str(root / 'v')
            with (root / 'host.log').open('wb') as log:
                process = subprocess.Popen(list(map(str, command)), env=trial_env, stdout=log, stderr=log, start_new_session=True)
                attempt['supervisor_pid'] = process.pid
                try:
                    until = time.monotonic() + args.deadline
                    console = root / 'm/console.log'
                    while time.monotonic() < until:
                        if process.poll() is not None:
                            attempt['status'] = 'supervisor_exit'
                            attempt['exit_code'] = process.returncode
                            break
                        if console.exists() and 'bctl-guest: ready' in console.read_text(errors='replace'):
                            attempt['report'] = report(root / 's/vsock.sock_9223')
                            attempt['status'] = 'ready' if all(line in attempt['report'] for line in ('browser ok', 'sandbox ok', 'profile ok')) else 'incomplete_report'
                            break
                        time.sleep(0.02)
                    else:
                        attempt['status'] = 'readiness_timeout'
                    if attempt['status'] != 'ready':
                        children = m.invoke(['pgrep', '-P', str(process.pid)]).split()
                        attempt['child_pids'] = children
                        for pid in children:
                            result = subprocess.run(['sample', pid, '1', '-file', root / f'host-sample-{pid}.txt'], capture_output=True, text=True, timeout=10)
                            attempt.setdefault('samples', []).append({'pid': int(pid), 'code': result.returncode, 'diagnostic': result.stdout + result.stderr})
                    if args.registers and (index == 0 or attempt['status'] != 'ready'):
                        attempt['control'] = []
                        for _ in range(2):
                            attempt['control'].append(control(root / 'v', 'PAUSE'))
                            attempt['control'].append(control(root / 'v', 'BOOT_TRACE'))
                            attempt['control'].append(control(root / 'v', 'RESUME'))
                            time.sleep(0.3)
                        attempt['ready_after_pause'] = console.exists() and 'bctl-guest: ready' in console.read_text(errors='replace')
                except BaseException as error:
                    attempt['error'] = str(error)
                    raise
                finally:
                    try:
                        stop(process)
                    finally:
                        for name in ('console.log', 'supervisor.log'):
                            filename = root / 'm' / name
                            if filename.exists():
                                attempt[name] = filename.read_text(errors='replace')
                        m.save_results(args.out, receipt)
            print(index, cpus, attempt['status'], flush=True)
            if attempt['status'] != 'ready':
                break
        receipt['changed_inputs'] = [str(path) for path in inputs if digest(path) != receipt['inputs_sha256'][str(path)]]
        receipt['completed'] = True
    finally:
        m.save_results(args.out, receipt)


if __name__ == '__main__':
    with m.interruptions():
        main()
