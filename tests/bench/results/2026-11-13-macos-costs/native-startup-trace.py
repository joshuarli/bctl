import argparse
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import time

ROOT = Path.cwd().resolve()
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m


def main():
    home = Path(tempfile.mkdtemp(prefix='ntrace-', dir=m.SCRATCH))
    receipt = {'completed': False, 'attempts': [], 'method': 'diagnostic only; fresh native profiles; software rendering; startup,toplevel tracing for two seconds; no performance claim'}
    output = ROOT / '.work/tmp/native-startup-trace.json'
    try:
        for index in range(5):
            path = home / str(index)
            trace = path / 'chromium-startup.json'
            args = argparse.Namespace(runtime='nanos', native_binary=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'),
                native_arg=['--disable-gpu', '--trace-startup=startup,toplevel', '--trace-startup-duration=2',
                            '--trace-startup-format=json', f'--trace-startup-file={trace}'])
            trial = m.Trial(args, path, 'native', resource_pass=False)
            row = {'round': index, 'gate_checks': [], 'trace': str(trace.relative_to(ROOT))}
            receipt['attempts'].append(row)
            try:
                before = m.gate(row['gate_checks'])
                row['startup_ms'] = trial.start()
                row['clean'] = m.gate(row['gate_checks'], True) and before
                row['browser'] = trial.version
                deadline = time.monotonic() + 10
                while True:
                    try:
                        data = trace.read_bytes()
                        row['trace_events'] = len(json.loads(data)['traceEvents'])
                        row['trace_sha256'] = hashlib.sha256(data).hexdigest()
                        break
                    except (OSError, ValueError):
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(0.05)
                row['status'] = 'pass'
            except BaseException as error:
                row['status'] = 'fail'
                row['error'] = str(error)
                raise
            finally:
                trial.close()
                m.save_results(output, receipt)
        receipt['completed'] = True
    finally:
        m.save_results(output, receipt)


if __name__ == '__main__':
    with m.interruptions():
        main()
