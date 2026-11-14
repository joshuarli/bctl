import argparse
import concurrent.futures
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m
import macos_warm_cdp as warm


def parse_footprint(output, expected):
    expected = set(expected)
    physical = m.physical_footprint_sample(output, expected)
    header = re.compile(r'^.*\[(\d+)\]:[^\n]*Footprint:\s+([\d,]+) B', re.M)
    matches = list(header.finditer(output))
    processes = {}
    categories = {}
    peak = 0
    for index, match in enumerate(matches):
        pid = int(match.group(1))
        if pid in processes:
            raise ValueError('duplicate process block')
        block = output[match.end():matches[index + 1].start() if index + 1 < len(matches) else len(output)]
        peaks = re.findall(r'^\s*phys_footprint_peak:\s*([\d,]+) B', block, re.M)
        if len(peaks) != 1:
            raise ValueError('missing process peak')
        process_categories = {}
        category_block = re.split(r'^\s*(?:Auxiliary data:|Summary Footprint:)', block, maxsplit=1, flags=re.M)[0]
        for dirty, clean, reclaimable, regions, name in re.findall(
                r'^\s*([\d,]+) B\s+([\d,]+) B\s+([\d,]+) B\s+([\d,]+)\s+(.+?)\s*$', category_block, re.M):
            values = tuple(int(value.replace(',', '')) for value in (dirty, clean, reclaimable))
            old = process_categories.get(name, (0, 0, 0))
            process_categories[name] = tuple(a + b for a, b in zip(old, values))
        if 'TOTAL' not in process_categories:
            raise ValueError('missing category total')
        processes[pid] = {'physical_bytes': int(match.group(2).replace(',', '')),
                          'peak_physical_bytes': int(peaks[0].replace(',', '')),
                          'categories': process_categories}
        peak += processes[pid]['peak_physical_bytes']
        for name, values in process_categories.items():
            if name != 'TOTAL':
                old = categories.get(name, (0, 0, 0))
                categories[name] = tuple(a + b for a, b in zip(old, values))
    totals = [sum(row['categories']['TOTAL'][i] for row in processes.values()) for i in range(3)]
    return {'physical_bytes': physical['physical_bytes'], 'peak_physical_bytes': peak,
            'peak_method': 'sum of per-process peaks; peaks may occur at different times and shared pages may overlap',
            'dirty_bytes': totals[0], 'clean_bytes': totals[1], 'reclaimable_bytes': totals[2],
            'mapped_file_clean_bytes': sum(v[1] for k, v in categories.items() if 'mapped file' in k.lower()),
            'text_clean_bytes': sum(v[1] for k, v in categories.items() if k.startswith('__TEXT')),
            'category_method': 'sum of per-process categories; shared pages may overlap', 'processes': processes}


def density_metrics(physical_bytes, count, baseline_bytes, completed, elapsed_s):
    if physical_bytes <= 0 or count <= 0 or elapsed_s <= 0 or completed < 0:
        raise ValueError('invalid density inputs')
    if count > 1 and baseline_bytes is None:
        raise ValueError('single-instance baseline required')
    return {'per_instance_bytes': physical_bytes / count,
            'marginal_bytes': (physical_bytes - baseline_bytes) / (count - 1) if count > 1 else None,
            'workflows_per_sec_per_gib': (completed / elapsed_s) / (physical_bytes / 2**30)}


def shareable_files(root):
    root = Path(root)
    if not root.is_dir():
        raise ValueError('browser resource directory does not exist')
    files = []
    seen = set()
    for path in root.rglob('*'):
        if not path.is_file() or path.is_symlink():
            continue
        stat = path.stat()
        key = (stat.st_dev, stat.st_ino)
        if key in seen:
            continue
        with path.open('rb') as stream:
            elf = stream.read(4) == b'\x7fELF'
        if elf or path.suffix == '.pak' or path.name == 'icudtl.dat':
            seen.add(key)
            files.append({'path': str(path.relative_to(root)), 'bytes': stat.st_size})
    return {'files': files, 'file_bytes': sum(row['bytes'] for row in files),
            'meaning': 'file sizes are an upper bound, not observed resident or duplicated pages'}


def process_ids(trials):
    rows = []
    for line in m.process_table().splitlines():
        pid, ppid, command = line.strip().split(None, 2)
        if Path(command.split()[0]).name not in {'ps', 'footprint', 'lsof'}:
            rows.append((int(pid), int(ppid), command))
    roots = set()
    for trial in trials:
        trial.discover_daemon()
        roots.update(trial.roots)
        roots.update(pid for pid, _, command in rows if str(trial.path) in command)
        for pidfile in trial.socket_dir.rglob('*.pid'):
            roots.add(int(pidfile.read_text().strip()))
    return m.tree(roots, rows) & {row[0] for row in rows}


def collect_footprint(trials):
    pids = process_ids(trials)
    if not pids:
        raise RuntimeError('no session processes found')
    command = ['footprint', '-f', 'bytes']
    for pid in sorted(pids):
        command.extend(['-p', str(pid)])
    return parse_footprint(m.invoke(command, timeout=30), pids)


def workflow(trial, sites, setup=False):
    env = trial.env
    def run(*args):
        return m.invoke([m.AGENT, *args], env=env)
    if setup:
        trial.start()
        with urllib.request.urlopen(trial.endpoint + '/json/version', timeout=5) as response:
            trial.env['AGENT_BROWSER_CDP'] = json.load(response)['webSocketDebuggerUrl']
    run('open', sites.form + '/form')
    run('snapshot', '-i')
    run('fill', '#name', 'Grace Hopper')
    run('click', '#submit')
    run('wait', '--text', 'sent Grace Hopper')
    run('screenshot', str(trial.path / 'form.png'))
    if 'sent Grace Hopper' not in run('get', 'text', '#out'):
        raise RuntimeError('form submission failed')


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--counts', nargs='+', type=int, default=[1, 2, 4])
    p.add_argument('--sides', nargs='+', choices=['native', 'native-software', 'bctl'], default=['native', 'native-software', 'bctl'])
    p.add_argument('--balloon-reclaim', action='store_true')
    p.add_argument('--browser-files', type=Path)
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    if 1 not in args.counts or any(n < 1 for n in args.counts) or len(set(args.counts)) != len(args.counts):
        p.error('counts must be unique positive integers including 1')
    m.SCRATCH.mkdir(parents=True, exist_ok=True)
    home = Path(tempfile.mkdtemp(prefix='density-', dir=m.SCRATCH))
    receipt = {'attempts': [], 'guest_internal_rss_added_to_host': False,
               'guest_file_sizes': shareable_files(args.browser_files) if args.browser_files else None}
    with warm.Sites() as sites:
        for side in args.sides:
            baseline = None
            for count in sorted(args.counts):
                row = {'side': side, 'count': count, 'gate_checks': []}
                receipt['attempts'].append(row)
                trials = []
                try:
                    if not m.gate(row['gate_checks']):
                        row['status'] = 'deferred_host_busy'
                        continue
                    try:
                        for index in range(count):
                            options = argparse.Namespace(runtime='nanos', native_arg=['--disable-gpu'] if side == 'native-software' else [], native_binary=warm.NATIVE_BINARY)
                            trial = m.Trial(options, home / f'{side}-{count}-{index}', 'native' if side == 'native-software' else side, resource_pass=True)
                            if args.balloon_reclaim and side == 'bctl':
                                trial.env['SMOLVM_BALLOON_RECLAIM'] = '1'
                            trials.append(trial)
                        with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
                            list(pool.map(lambda trial: workflow(trial, sites, True), trials))
                        sample = collect_footprint(trials)
                        started = time.monotonic()
                        with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
                            list(pool.map(lambda trial: workflow(trial, sites), trials))
                        elapsed = time.monotonic() - started
                        if count == 1:
                            baseline = sample['physical_bytes']
                        row.update(sample)
                        row.update(density_metrics(sample['physical_bytes'], count, baseline, count, elapsed))
                        row['completed_workflows'] = count
                        row['elapsed_s'] = elapsed
                    finally:
                        clean = m.gate(row['gate_checks'], True)
                    row['status'] = 'pass' if clean else 'discarded_host_busy'
                finally:
                    for trial in trials:
                        try:
                            subprocess.run([m.AGENT, 'close'], env=trial.env, capture_output=True, timeout=40)
                        finally:
                            trial.close()
                    m.save_results(args.out, receipt)


if __name__ == '__main__':
    main()
