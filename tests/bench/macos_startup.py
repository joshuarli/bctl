"""Profile Nanos startup boundaries separately from acceptance timings."""

import argparse
import contextlib
import hashlib
import json
from pathlib import Path
import re
import shutil
import sys
import tempfile
import threading
import time

import macos_workflows as workflows


def events(text):
    return [{'component': component, 'stage': stage, 'epoch_us': int(epoch),
             'elapsed_us': int(elapsed) if elapsed else None}
            for component, stage, epoch, elapsed in re.findall(
                r'^bctl-startup (\S+) (.+?) epoch_us=(\d+)(?: elapsed_us=(\d+))?$', text, re.MULTILINE)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--image', type=Path, default=workflows.NANOS_IMAGE)
    parser.add_argument('--chromium-trace', action='store_true', help='require a diagnostic image with BCTL_TRACE_CHROMIUM=1; retain its startup trace')
    args = parser.parse_args()
    if sys.platform != 'darwin' or args.rounds < 1:
        parser.error('requires macOS and positive rounds')
    workflows.SCRATCH.mkdir(parents=True, exist_ok=True)
    home = Path(tempfile.mkdtemp(prefix='startup-', dir=workflows.SCRATCH))
    inputs = [Path(__file__), Path(workflows.__file__), workflows.BCTL, workflows.ROOT / '.work/runtime/nanos-krun/nanos-krun',
              workflows.ROOT / '.work/runtime/nanos-krun/lib/libkrun.dylib',
              workflows.ROOT / '.work/runtime/nanos-krun/kernel.img',
              args.image.resolve(), args.image.resolve().with_suffix('.profile.img')]
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    receipt = {'completed': False, 'method': 'diagnostic only; startup trace and libkrun timing enabled; console observed every 1 ms; wall timestamps correlate host processes and observations, not guest clocks',
               'bctl_commit': workflows.invoke(['git', '-C', workflows.ROOT, 'rev-parse', 'HEAD']).strip(),
               'inputs_sha256': {str(path): digest(path) for path in inputs}, 'attempts': []}
    trial_args = argparse.Namespace(native_binary=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'), native_arg=[], runtime='nanos')
    try:
        for index in range(args.rounds):
            trial = workflows.Trial(trial_args, home / str(index), 'bctl', resource_pass=False, image_path=args.image)
            trial.env |= {'BCTL_STARTUP_TRACE': '1', 'LIBKRUN_TIMING': '1'}
            attempt = {'round': index, 'gate_checks': [], 'artifacts': str(trial.path.relative_to(workflows.ROOT)), 'console_observations': []}
            receipt['attempts'].append(attempt)
            done = threading.Event()
            observed_logs = {}
            def observe():
                seen = set()
                while not done.is_set():
                    for path in (trial.path / 'bctl/machines').glob('*/*.log'):
                        with contextlib.suppress(OSError):
                            text = path.read_text(errors='replace')
                            observed_logs[path.name] = text
                            if path.name != 'console.log':
                                continue
                            for marker in ['NET:', 'bctl-guest: ready']:
                                if marker in text and marker not in seen:
                                    seen.add(marker)
                                    attempt['console_observations'].append({'marker': marker, 'epoch_us': time.time_ns() // 1000})
                    done.wait(0.001)
            observer = threading.Thread(target=observe)
            try:
                clean = workflows.gate(attempt['gate_checks'])
                observer.start()
                attempt['begin_epoch_us'] = time.time_ns() // 1000
                attempt['startup_ms'] = trial.start()
                attempt['end_epoch_us'] = time.time_ns() // 1000
                attempt['browser'] = trial.version
                attempt['clean'] = workflows.gate(attempt['gate_checks'], True) and clean
                if args.chromium_trace:
                    trace = trial.path / 'bctl/files/bench/chromium-startup.json'
                    deadline = time.monotonic() + 10
                    while True:
                        try:
                            trace_data = json.loads(trace.read_bytes())
                            if not trace_data['traceEvents']:
                                raise ValueError('Chromium recorded no trace events')
                            attempt['chromium_trace_events'] = len(trace_data['traceEvents'])
                            attempt['chromium_trace_sha256'] = digest(trace)
                            attempt['chromium_trace'] = str(trace.relative_to(workflows.ROOT))
                            break
                        except (OSError, ValueError):
                            if time.monotonic() >= deadline:
                                raise
                            time.sleep(0.05)
                attempt['status'] = 'pass'
            except Exception as error:
                attempt['status'] = 'fail'
                attempt['error'] = str(error)
                raise
            finally:
                done.set()
                if observer.is_alive():
                    observer.join(timeout=5)
                logs = trial.path / 'logs'
                logs.mkdir()
                for name, text in observed_logs.items():
                    (logs / name).write_text(text)
                for path in (trial.path / 'bctl').rglob('*.log'):
                    shutil.copyfile(path, logs / path.name)
                attempt['events'] = events('\n'.join(path.read_text(errors='replace') for path in logs.glob('*.log')))
                attempt['guest_events'] = [{'component': 'kernel' if kernel else 'guest', 'stage': stage, 'monotonic_us': int(at)}
                    for kernel, stage, at in re.findall(r'(?:(hydrogen)-startup|bctl-startup guest) (\S+) monotonic_us=(\d+)',
                        '\n'.join(path.read_text(errors='replace') for path in logs.glob('*.log')))]
                trial.close()
                workflows.save_results(args.out, receipt)
            required = {('supervisor', 'profile_ready'), ('supervisor', 'root_disk_ready'), ('runner', 'enter_vm'), ('readiness', 'ready')}
            if not required <= {(event['component'], event['stage']) for event in attempt['events']}:
                raise RuntimeError('startup instrumentation missing from binaries; rebuild bctl and nanos-krun')
            print(f'round {index}: {attempt["startup_ms"]:.1f} ms, clean={attempt["clean"]}', flush=True)
        receipt['changed_inputs'] = [str(path) for path in inputs if digest(path) != receipt['inputs_sha256'][str(path)]]
        if receipt['changed_inputs']:
            raise RuntimeError('profile inputs changed')
        receipt['completed'] = True
    finally:
        workflows.save_results(args.out, receipt)


if __name__ == '__main__':
    with workflows.interruptions():
        main()
