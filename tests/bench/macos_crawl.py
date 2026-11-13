"""Alternate sustained crawls under a sampled aggregate footprint budget."""

import argparse
import contextlib
import functools
import hashlib
import http.server
import json
from pathlib import Path
import statistics
import signal
import subprocess
import sys
import tempfile
import threading
import time

import crawl_site
import macos_workflows as workflows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pages', type=int, default=16, help='pages in each independent crawl')
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--slots', type=int, default=4)
    parser.add_argument('--budget-mib', type=int, default=2048)
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--resource-rounds', type=int, default=5)
    parser.add_argument('--native-arg', action='append', default=[])
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'darwin' or min(args.pages, args.jobs, args.slots, args.budget_mib) < 1 or min(args.rounds, args.resource_rounds) < 0 or args.rounds + args.resource_rounds == 0:
        parser.error('requires macOS, positive workload sizes, and nonnegative rounds')
    workflows.SCRATCH.mkdir(parents=True, exist_ok=True)
    home = Path(tempfile.mkdtemp(prefix='crawl-', dir=workflows.SCRATCH))
    site = home / 'site'
    crawl_site.write(site, args.pages)
    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    inputs = [Path(__file__), Path(__file__).with_name('macos_crawl_worker.py'), Path(__file__).with_name('crawl_site.py'),
              Path(workflows.__file__), workflows.BCTL, workflows.ROOT / '.work/images/nanos-helium.img',
              workflows.ROOT / '.work/runtime/nanos-krun/nanos-krun', workflows.ROOT / '.work/runtime/nanos-krun/lib/libkrun.dylib',
              workflows.ROOT.parent / 'shadowdriver/py/python/shadowdriver/_native.abi3.so',
              Path('/Applications/Chromium.app/Contents/MacOS/Chromium')]
    receipt = {'completed': False, 'configuration': vars(args) | {'out': str(args.out)}, 'attempts': [],
        'bctl_commit': workflows.invoke(['git', '-C', workflows.ROOT, 'rev-parse', 'HEAD']).strip(),
        'shadowdriver_commit': workflows.invoke(['git', '-C', workflows.ROOT.parent / 'shadowdriver', 'rev-parse', 'HEAD']).strip(),
        'inputs_sha256': {str(path): digest(path) for path in inputs},
        'method': 'fresh profiles; two independent concurrent crawls by default; complete capture, asset and state checks, and fresh native offline replay; capture browsers remain alive through replay; latency and resource passes separate; alternating backends; sampled budget, not an OS-enforced cap',
        'summary': {}}
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=site))
    serving = threading.Thread(target=server.serve_forever)
    serving.start()
    try:
        for phase, rounds in [('latency', args.rounds), ('resources', args.resource_rounds)]:
            for index in range(rounds):
                for side in (['native', 'bctl'] if index % 2 == 0 else ['bctl', 'native']):
                    path = home / f'{phase}-{index}-{side}'
                    path.mkdir()
                    group = argparse.Namespace(path=path, socket_dir=path, roots=set())
                    attempt = {'phase': phase, 'round': index, 'side': side, 'artifacts': str(path.relative_to(workflows.ROOT)), 'gate_checks': []}
                    receipt['attempts'].append(attempt)
                    children = []
                    logs = []
                    try:
                        if phase == 'latency' and not workflows.gate(attempt['gate_checks']):
                            attempt['status'] = 'deferred_host_busy'
                            continue
                        sampler = workflows.Resources(group) if phase == 'resources' else contextlib.nullcontext()
                        print(f'{phase} {index} {side}: {args.jobs} crawls x {args.pages} pages', flush=True)
                        with sampler:
                            started = time.perf_counter()
                            for job in range(args.jobs):
                                command = [sys.executable, Path(__file__).with_name('macos_crawl_worker.py'), '--path', path / str(job),
                                    '--side', side, '--url', f'http://127.0.0.1:{server.server_port}/page-0.html', '--pages', str(args.pages), '--slots', str(args.slots)]
                                command += [f'--native-arg={arg}' for arg in args.native_arg]
                                log = (path / f'worker-{job}.log').open('wb')
                                logs.append(log)
                                child = subprocess.Popen([str(arg) for arg in command], stdout=log, stderr=log, start_new_session=True)
                                children.append(child)
                                group.roots.add(child.pid)
                            for child in children:
                                if child.wait(timeout=max(0, started + 1800 - time.perf_counter())):
                                    raise RuntimeError(f'crawl worker failed; see {path}')
                            attempt['total_s'] = time.perf_counter() - started
                        attempt['workers'] = [json.loads((path / str(job) / 'result.json').read_text()) for job in range(args.jobs)]
                        if any(worker['status'] != 'pass' for worker in attempt['workers']):
                            raise RuntimeError('a crawl did not pass verification')
                        attempt['completed_pages'] = args.jobs * args.pages
                        attempt['pages_per_s'] = attempt['completed_pages'] / attempt['total_s']
                        attempt['status'] = 'pass'
                        if phase == 'latency':
                            attempt['timing_status'] = 'clean' if workflows.gate(attempt['gate_checks'], True) else 'discarded_host_busy'
                        else:
                            attempt['resource_samples'] = sampler.samples
                            attempt['resource_errors'] = sampler.errors
                            if not sampler.samples:
                                raise RuntimeError('no complete resource samples')
                            attempt['sampled_peak_physical_bytes'] = max(sample['physical_bytes'] for sample in sampler.samples)
                            attempt['within_sampled_budget'] = attempt['sampled_peak_physical_bytes'] <= args.budget_mib * 2**20
                            if not attempt['within_sampled_budget']:
                                attempt['status'] = 'over_budget'
                    except Exception as error:
                        attempt['status'] = 'fail'
                        attempt['error'] = str(error)
                        raise
                    finally:
                        for child in children:
                            if child.poll() is None:
                                child.send_signal(signal.SIGINT)
                        for child in children:
                            try:
                                child.wait(timeout=40)
                            except subprocess.TimeoutExpired:
                                workflows.stop(child)
                        for log in logs:
                            log.close()
                        workflows.save_results(args.out, receipt)
        receipt['changed_inputs'] = [str(path) for path in inputs if digest(path) != receipt['inputs_sha256'][str(path)]]
        if receipt['changed_inputs']:
            raise RuntimeError('crawl inputs changed during measurement')
        for side in ['native', 'bctl']:
            rows = [row for row in receipt['attempts'] if row['side'] == side and row['status'] == 'pass']
            metrics = {}
            for metric, phase in [('total_s', 'latency'), ('pages_per_s', 'latency'), ('sampled_peak_physical_bytes', 'resources')]:
                values = [row[metric] for row in rows if row['phase'] == phase and (phase != 'latency' or row['timing_status'] == 'clean')]
                metrics[metric] = {'n': len(values), 'median': statistics.median(values) if len(values) >= 5 else None,
                                   'min': min(values) if values else None, 'max': max(values) if values else None}
            receipt['summary'][side] = metrics
        receipt['completed'] = True
    finally:
        server.shutdown()
        server.server_close()
        serving.join(timeout=5)
        workflows.save_results(args.out, receipt)
    print(args.out, flush=True)


if __name__ == '__main__':
    with workflows.interruptions():
        main()
