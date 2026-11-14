"""Build, gate, and A/B-measure Nanos kernel or image candidates against a baseline on macOS.

Each ref is built in a throwaway worktree into .work/exp/builds/<key>, keyed by its kernel, guest,
images, and runtime trees plus KERNEL_TEST_CFLAGS, so unchanged inputs are never rebuilt. A
candidate must pass its tier's kernel gates before it is measured. Measurement interleaves the
variants round-robin, checks host load before and after every run, and discards and repeats a
run that overlapped load. A metric is called improved or regressed only when at least five run
medians per variant do not overlap and the medians differ by more than 3%.
"""

import argparse
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_warm_cdp as w
import macos_workflows as m

EXP = ROOT / '.work/exp'
GATES = {'edit': ['latency', 'probe'], 'change': ['latency', 'probe', 'harness']}
INPUT_TREES = ('kernel', 'guest', 'images', 'runtime')
SHARED_KERNEL_WORK = ('dl',)
MIN_RUNS = 5
MIN_EFFECT = 0.03
STEPS = ('form click', 'form screenshot', 'news open', 'news click', 'news screenshot', 'FORM TOTAL', 'NEWS TOTAL')


def git(*args, cwd=ROOT):
    return subprocess.run(['git', '-C', str(cwd), *args], check=True, capture_output=True, text=True).stdout.strip()


def build_key(ref, cflags):
    trees = ' '.join(git('rev-parse', f'{ref}:{path}') for path in INPUT_TREES)
    return hashlib.sha256(f'{trees}|{cflags}'.encode()).hexdigest()[:16]


def logged(cmd, log, env, cwd):
    with open(log, 'wb') as out:
        if subprocess.run(cmd, stdout=out, stderr=subprocess.STDOUT, env=env, cwd=cwd).returncode:
            raise SystemExit(f'{" ".join(map(str, cmd))} failed; see {log}')


def latency_percentiles(runs):
    found = {}
    for run in sorted(runs.glob('*-wakeup_latency-smp*')):
        smp = re.search(r'-smp(\d+)-', run.name).group(1)
        detail = re.search(r'detail=(\S+)', (run / 'serial.log').read_text(errors='replace'))
        if detail:
            found[f'smp{smp}'] = dict((k, float(v)) for k, v in (kv.split('=') for kv in detail.group(1).split(',')))
    return found


def build(ref, cflags, tier, archive):
    """Return the build directory for `ref`, building and gating it unless a cached build passed `tier`."""
    key = build_key(ref, cflags)
    out = EXP / 'builds' / key
    meta_path = out / 'meta.json'
    if meta_path.exists():
        meta = json.loads(meta_path.read_text())
        if set(GATES[tier]) <= set(meta['gates']) | set(meta['gates_absent']):
            print(f'{ref}: reusing build {key}', flush=True)
            return out, meta
    commit = git('rev-parse', f'{ref}^{{commit}}')
    out.mkdir(parents=True, exist_ok=True)
    tree = EXP / 'wt' / key
    if tree.exists():
        subprocess.run(['git', '-C', str(ROOT), 'worktree', 'remove', str(tree)], check=True)
    git('worktree', 'add', '--detach', str(tree), commit)
    try:
        (tree / 'kernel/.work').mkdir(parents=True)
        for name in SHARED_KERNEL_WORK:
            source = ROOT / 'kernel/.work' / name
            if not source.exists():
                raise SystemExit(f'{source} is missing: run sh kernel/dev/bootstrap.sh in the main checkout first')
            subprocess.run(['cp', '-cR', source, tree / 'kernel/.work' / name], check=True)
        (tree / '.work/tmp').mkdir(parents=True)
        (tree / 'kernel/.work/tmp').mkdir()
        env = os.environ | {'TMPDIR': str(tree / '.work/tmp'), 'HYDROGEN_KRUN': str(ROOT / '.work/runtime/nanos-krun'),
                            'BCTL_BROWSER_ARCHIVE': str(archive)}
        env.pop('KERNEL_TEST_CFLAGS', None)
        if cflags:
            env['KERNEL_TEST_CFLAGS'] = cflags
        started = time.monotonic()
        print(f'{ref}: building {commit[:12]} as {key}', flush=True)
        logged(['sh', 'kernel/dev/bootstrap.sh'], out / 'bootstrap.log', env | {'TMPDIR': str(tree / 'kernel/.work/tmp')}, tree)
        logged(['sh', 'runtime/build-nanos-arm64.sh'], out / 'kernel.log', env, tree)
        logged(['sh', 'images/nanos-helium/build-macos.sh'], out / 'image.log', env, tree)
        for name in ('nanos-helium.img', 'nanos-helium.profile.img', 'nanos-helium.kernel.img'):
            subprocess.run(['cp', '-c', tree / '.work/images' / name, out / name], check=True)
        build_seconds = time.monotonic() - started
        gate_seconds, absent = {}, []
        suites = (tree / 'kernel/dev/test.sh').read_text()
        for gate in GATES[tier]:
            if f'\t{gate})' not in suites:
                print(f'{ref}: gate {gate} does not exist at this ref; recorded as absent', flush=True)
                absent.append(gate)
                continue
            print(f'{ref}: gate {gate}', flush=True)
            started = time.monotonic()
            gate_env = env | {'TMPDIR': str(tree / 'kernel/.work/tmp')}
            logged(['sh', 'kernel/dev/test.sh', gate], out / f'gate-{gate}.log', gate_env, tree)
            if 'test: ok' not in (out / f'gate-{gate}.log').read_text(errors='replace'):
                raise SystemExit(f'{ref}: gate {gate} did not report test: ok; see {out}/gate-{gate}.log')
            gate_seconds[gate] = round(time.monotonic() - started, 1)
        meta = {'ref': ref, 'commit': commit, 'key': key, 'cflags': cflags,
                'gates': [g for g in GATES[tier] if g not in absent], 'gates_absent': absent,
                'build_seconds': round(build_seconds, 1), 'gate_seconds': gate_seconds,
                'latency_us': latency_percentiles(tree / 'kernel/.work/runs')}
        meta_path.write_text(json.dumps(meta, indent=1))
        return out, meta
    finally:
        subprocess.run(['git', '-C', str(ROOT), 'worktree', 'remove', str(tree)], check=True)


def quiet_host(wait_seconds):
    deadline = time.monotonic() + wait_seconds
    while subprocess.run(['perf-gate-ready'], capture_output=True).returncode:
        if time.monotonic() > deadline:
            raise SystemExit(f'host stayed busy for {wait_seconds} s; no timing was taken')
        time.sleep(30)


def gated_measure(variant, image, sites, iterations, attempts, wait_seconds):
    """One measurement that passed the host-load check before and after, plus the count of discarded runs."""
    discarded = 0
    for _ in range(attempts):
        quiet_host(wait_seconds)
        home = Path(tempfile.mkdtemp(prefix=f'exp-{variant}-', dir=m.SCRATCH))
        result = w.measure('bctl', sites, home / 'bctl', iterations, image)
        if subprocess.run(['perf-gate-ready', '-1'], capture_output=True).returncode == 0:
            return result, discarded
        discarded += 1
        print(f'{variant}: run overlapped host load; discarded', flush=True)
    raise SystemExit(f'{variant}: {attempts} runs in a row overlapped host load; no verdict')


def run_medians(runs, metric):
    kind, name = metric
    if kind == 'step':
        return [statistics.median(r['rows'][name]['browser']) for r in runs]
    if kind == 'wall':
        return [statistics.median(r['rows'][name]['wall']) for r in runs]
    if kind == 'method':
        return [r['methods'][name] for r in runs if name in r['methods']]
    return [r['vm_cpu_seconds'] for r in runs]


def verdict(baseline, candidate):
    """Lower is better. A change needs MIN_RUNS run medians per side that do not overlap and medians that
    differ by more than MIN_EFFECT: two identical builds then disagree about 0.8% of the time per metric."""
    if not baseline or not candidate:
        return 'unmeasured'
    if min(len(baseline), len(candidate)) < MIN_RUNS:
        return 'too few runs'
    effect = abs(statistics.median(candidate) - statistics.median(baseline)) / statistics.median(baseline)
    if effect <= MIN_EFFECT:
        return 'no change established'
    if max(candidate) < min(baseline):
        return 'improved'
    if min(candidate) > max(baseline):
        return 'regressed'
    return 'no change established'


def summarize(results, names):
    metrics = ([('step', s) for s in STEPS] + [('wall', 'FORM TOTAL'), ('wall', 'NEWS TOTAL')]
               + [('method', meth) for meth in w.METHODS] + [('cpu', 'VM CPU seconds')])
    summary = []
    base = results[names[0]]
    for metric in metrics:
        row = {'metric': f'{metric[0]}:{metric[1]}', names[0]: run_medians(base, metric)}
        for name in names[1:]:
            row[name] = run_medians(results[name], metric)
            row[f'{name} verdict'] = verdict(row[names[0]], row[name])
        summary.append(row)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--candidate', action='append', required=True, help='git ref to compare; repeat for several')
    parser.add_argument('--baseline', default='main')
    parser.add_argument('--name', required=True, help='short experiment name for the receipt and ledger')
    parser.add_argument('--tier', choices=sorted(GATES), default='edit')
    parser.add_argument('--pairs', type=int, default=MIN_RUNS)
    parser.add_argument('--iterations', type=int, default=5)
    parser.add_argument('--cflags', default='', help='KERNEL_TEST_CFLAGS for every variant, such as -DHYDROGEN_FLUSH_PROFILE')
    parser.add_argument('--bctl', type=Path, default=m.BCTL)
    parser.add_argument('--archive', type=Path, default=ROOT / '.work/archives/helium-r6')
    parser.add_argument('--attempts', type=int, default=3, help='runs to try before giving up on a busy host')
    parser.add_argument('--wait', type=int, default=900, help='seconds to wait for a quiet host before each run')
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    if not args.bctl.exists():
        raise SystemExit(f'{args.bctl} is missing: cargo build --release -p bctl')
    m.BCTL = args.bctl.resolve()
    names = ['baseline'] + [f'candidate{i + 1}' for i in range(len(args.candidate))]
    refs = dict(zip(names, [args.baseline, *args.candidate]))
    builds = {name: build(ref, args.cflags, args.tier, args.archive.resolve()) for name, ref in refs.items()}
    results = {name: [] for name in names}
    discarded = 0
    with w.Sites() as sites:
        for round_index in range(args.pairs):
            order = names[round_index % len(names):] + names[:round_index % len(names)]
            for name in order:
                result, lost = gated_measure(name, builds[name][0] / 'nanos-helium.img', sites, args.iterations,
                                             args.attempts, args.wait)
                discarded += lost
                results[name].append(result)
                print(f'{name} round {round_index + 1}: form {statistics.median(result["rows"]["FORM TOTAL"]["browser"]):.1f} ms'
                      f', news {statistics.median(result["rows"]["NEWS TOTAL"]["browser"]):.1f} ms', flush=True)
    summary = summarize(results, names)
    receipt = {'experiment': args.name, 'started': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'tier': args.tier,
               'pairs': args.pairs, 'iterations': args.iterations, 'cflags': args.cflags,
               'discarded_runs': discarded, 'host': subprocess.run(['sysctl', '-n', 'hw.model'], capture_output=True, text=True).stdout.strip(),
               'variants': {name: builds[name][1] for name in names}, 'summary': summary, 'runs': results}
    out = args.out or EXP / 'receipts' / f'{time.strftime("%Y%m%dT%H%M%S")}-{args.name}.json'
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(receipt, indent=1))
    width = max(len(r['metric']) for r in summary)
    print(f'\n{"metric":{width}}  ' + '  '.join(f'{n:>22}' for n in names))
    for row in summary:
        cells = [f'{statistics.median(row[names[0]]):10.2f}' if row[names[0]] else '         –']
        for name in names[1:]:
            med = f'{statistics.median(row[name]):.2f}' if row[name] else '–'
            label = {'no change established': 'same', 'too few runs': 'few runs'}.get(row[f'{name} verdict'], row[f'{name} verdict'])
            cells.append(f'{med:>10} {label:>11}')
        print(f'{row["metric"]:{width}}  ' + '  '.join(f'{c:>22}' for c in cells))
    for name in names:
        lat = builds[name][1].get('latency_us', {}).get('smp2')
        if lat:
            print(f'{name} wakeup latency smp2: p50 {lat["p50_us"]} us, p99 {lat["p99_us"]} us')
    print(f'\nreceipt {out}; discarded runs {discarded}')
    for name in names:
        if builds[name][1]['gates_absent']:
            print(f'{name}: gates not run because they do not exist at {builds[name][1]["commit"][:12]}: '
                  f'{", ".join(builds[name][1]["gates_absent"])}')
    for name in names[1:]:
        verdicts = {r['metric']: r[f'{name} verdict'] for r in summary}
        status = 'Adopted?' if 'improved' in verdicts.values() and 'regressed' not in verdicts.values() else 'Rejected?'
        print(f'ledger: | {args.name} ({builds[name][1]["commit"][:8]}) | form {verdicts["step:FORM TOTAL"]}, '
              f'news {verdicts["step:NEWS TOTAL"]}, VM CPU {verdicts["cpu:VM CPU seconds"]} | {status} | receipt | |')


if __name__ == '__main__':
    main()
