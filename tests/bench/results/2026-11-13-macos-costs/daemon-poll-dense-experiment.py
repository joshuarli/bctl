"""Compare daemon discovery intervals on the complete Shadowdriver form."""

import argparse
import contextlib
import hashlib
import json
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path.cwd().resolve()
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m
from macos_cpu import trial_snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--phases', nargs='+', choices=['daemon', 'latency', 'resources', 'cpu'], default=['daemon', 'latency', 'resources', 'cpu'])
    parser.add_argument('--resource-interval', type=float, default=0.25)
    args = parser.parse_args()
    if ROOT != m.ROOT or args.rounds < 1:
        parser.error('run from the bctl repository with positive rounds')
    binaries = {'baseline': ROOT / '.work/tmp/bctl-daemon-baseline', 'candidate': m.BCTL}
    trial_args = argparse.Namespace(runtime='nanos', native_arg=[], shadowdriver=ROOT.parent / 'shadowdriver',
        native_binary=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'))
    home = Path(tempfile.mkdtemp(prefix='dp-', dir=m.SCRATCH))
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    inputs = [Path(__file__).resolve(), Path(m.__file__), ROOT / 'tests/bench/macos_cpu.py', m.BCTL,
        ROOT / 'tests/bench/shadowdriver_workflow.mjs', Path(shutil.which('node')),
        trial_args.shadowdriver / 'ts/dist/src/browser.js', ROOT / '.work/runtime/nanos-krun/nanos-krun',
        ROOT / '.work/runtime/nanos-krun/lib/libkrun.dylib', ROOT / '.work/runtime/nanos-krun/firmware.fd',
        ROOT / '.work/runtime/nanos-arm64/hydrogen/nanos/output/platform/virt/bin/kernel.img',
        ROOT / '.work/images/nanos-helium.img.manifest', m.NANOS_IMAGE, m.NANOS_IMAGE.with_suffix('.profile.img'),
        ROOT / '.work/tmp/daemon-poll.diff', *binaries.values()]
    if digest(binaries['baseline']) == digest(binaries['candidate']):
        raise RuntimeError('expected distinct baseline and candidate executables')
    for binary in binaries.values():
        subprocess.run([binary, '--help'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    receipt = {'completed': False, 'attempts': [], 'summary': {},
        'phases': args.phases, 'resource_interval_s': args.resource_interval,
        'method': 'five alternating fresh-daemon and fresh-profile samples per executable and phase; daemon-only status checks plus complete Shadowdriver form with accessible textbox, input, click, submission and PNG checks; latency, footprint, and CPU separate; same Nanos image and runtime; baseline 100 ms discovery polling, candidate 10 ms; unchanged deadline and retry rules; warm binary and OS caches; two vCPUs and 4096 MiB',
        'binaries': {label: str(path) for label, path in binaries.items()},
        'source_commit': m.invoke(['git', 'rev-parse', 'HEAD']).strip(),
        'shadowdriver_commit': m.invoke(['git', '-C', trial_args.shadowdriver, 'rev-parse', 'HEAD']).strip(),
        'inputs_sha256': {str(path): digest(path) for path in inputs},
        'node_version': m.invoke(['node', '--version']).strip(),
        'host': {'model': m.invoke(['sysctl', '-n', 'hw.model']).strip(), 'cpus': int(m.invoke(['sysctl', '-n', 'hw.ncpu']))}}
    fixture_log = (home / 'fixture.log').open('wb')
    fixture = subprocess.Popen([sys.executable, ROOT / 'tests/interop/fixture.py'], stdout=subprocess.PIPE,
        stderr=fixture_log, start_new_session=True)
    try:
        port = int(fixture.stdout.readline())
        for phase in args.phases:
            for index in range(args.rounds):
                for variant in (list(binaries) if index % 2 == 0 else list(reversed(binaries))):
                    path = home / f'{phase[0]}{index}{variant[0]}'
                    row = {'phase': phase, 'round': index, 'variant': variant,
                           'artifacts': str(path.relative_to(ROOT)), 'gate_checks': []}
                    receipt['attempts'].append(row)
                    trial = m.Trial(trial_args, path, 'bctl', resource_pass=phase == 'resources')
                    try:
                        if phase != 'resources' and not m.gate(row['gate_checks']):
                            row['status'] = 'deferred_host_busy'
                            continue
                        print(phase, index, variant, flush=True)
                        sampler = m.Resources(trial, interval_s=args.resource_interval) if phase == 'resources' else contextlib.nullcontext()
                        with sampler:
                            if phase == 'cpu':
                                row['cpu_before'] = trial_snapshot(trial)
                            started = time.perf_counter()
                            if phase == 'daemon':
                                row['result'] = json.loads(m.invoke([binaries[variant], 'status', '--json'], env=trial.env))
                                if row['result']['sessions']:
                                    raise RuntimeError('fresh daemon unexpectedly owns sessions')
                                row['daemon_ready_ms'] = (time.perf_counter() - started) * 1000
                            else:
                                startup = time.perf_counter()
                                trial.vm_started = True
                                m.invoke([binaries[variant], 'up', 'bench', '--image', trial.image, '--allow-private', '127.0.0.1/32'], env=trial.env)
                                trial.endpoint = m.invoke([binaries[variant], 'cdp-url', 'bench'], env=trial.env).strip()
                                trial.version = json.load(urllib.request.urlopen(trial.endpoint + '/json/version', timeout=5))['Browser']
                                trial.env['BCTL_CDP_URL'] = trial.endpoint
                                if phase == 'resources':
                                    trial.discover_daemon()
                                row['startup_ms'] = (time.perf_counter() - startup) * 1000
                                row['result'] = m.shadowdriver(trial, port)
                            row['total_ms'] = (time.perf_counter() - started) * 1000
                            if phase == 'cpu':
                                row['cpu_after'] = trial_snapshot(trial)
                                row['cpu_s'] = row['cpu_after']['cpu_s'] - row['cpu_before']['cpu_s']
                                if row['cpu_s'] <= 0:
                                    raise RuntimeError('nonpositive CPU cost')
                            if phase == 'resources':
                                time.sleep(1)
                        row['status'] = 'pass'
                        if phase != 'resources':
                            row['timing_status'] = 'clean' if m.gate(row['gate_checks'], True) else 'discarded_host_busy'
                        else:
                            row['resource_samples'] = sampler.samples
                            row['resource_errors'] = sampler.errors
                            if not sampler.samples:
                                raise RuntimeError('no complete footprint sample')
                            row['sampled_peak_physical_bytes'] = max(sample['physical_bytes'] for sample in sampler.samples)
                    except BaseException as error:
                        row['status'] = 'fail'
                        row['error'] = str(error).replace(trial.endpoint, '<capability>') if trial.endpoint else str(error)
                        raise
                    finally:
                        trial.close()
                        m.save_results(args.out, receipt)
        receipt['changed_inputs'] = [str(path) for path in inputs if digest(path) != receipt['inputs_sha256'][str(path)]]
        receipt['shadowdriver_final_commit'] = m.invoke(['git', '-C', trial_args.shadowdriver, 'rev-parse', 'HEAD']).strip()
        if receipt['changed_inputs'] or receipt['shadowdriver_final_commit'] != receipt['shadowdriver_commit']:
            raise RuntimeError('experiment inputs changed')
        for variant in binaries:
            receipt['summary'][variant] = {}
            for phase, metric in [('daemon', 'daemon_ready_ms'), ('latency', 'total_ms'), ('latency', 'startup_ms'), ('cpu', 'cpu_s'), ('resources', 'sampled_peak_physical_bytes')]:
                values = [row[metric] for row in receipt['attempts'] if row['variant'] == variant and row['phase'] == phase
                    and row['status'] == 'pass' and (phase == 'resources' or row['timing_status'] == 'clean')]
                receipt['summary'][variant][metric] = {'n': len(values), 'median': statistics.median(values) if len(values) >= 5 else None,
                    'min': min(values) if values else None, 'max': max(values) if values else None}
        receipt['completed'] = True
    finally:
        m.stop(fixture)
        fixture_log.close()
        m.save_results(args.out, receipt)


if __name__ == '__main__':
    with m.interruptions():
        main()
