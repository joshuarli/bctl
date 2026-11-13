"""Browser-side time of the warm agent-browser workflows, measured at the CDP layer.

Both sides attach agent-browser with --cdp through cdp_relay.py. A step's browser-side time
is the union of CDP command intervals inside the step, so CLI spawn, daemon IPC and client
poll sleeps are excluded on both sides. `measure` is also used by kernel_experiment.py.
"""

import argparse
import functools
import http.server
import json
import re
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m

VENV_PY = ROOT / '.work/interop/venv/bin/python'
RELAY = ROOT / 'tests/bench/cdp_relay.py'
NATIVE_BINARY = Path('/Applications/Chromium.app/Contents/MacOS/Chromium')
METHODS = ('DOM.resolveNode', 'Runtime.callFunctionOn', 'Runtime.evaluate', 'Input.dispatchMouseEvent',
           'Page.captureScreenshot', 'Page.navigate', 'Accessibility.getFullAXTree')


def union_ms(intervals, lo, hi):
    spans = sorted((max(s, lo), min(e, hi)) for s, e in intervals if e > lo and s < hi)
    total, cur_s, cur_e = 0, None, None
    for s, e in spans:
        if cur_e is None or s > cur_e:
            if cur_e is not None:
                total += cur_e - cur_s
            cur_s, cur_e = s, e
        else:
            cur_e = max(cur_e, e)
    if cur_e is not None:
        total += cur_e - cur_s
    return total / 1e6


def cpu_seconds(pattern):
    total = 0.0
    for pid in subprocess.run(['pgrep', '-f', pattern], capture_output=True, text=True).stdout.split():
        out = subprocess.run(['ps', '-o', 'time=', '-p', pid], capture_output=True, text=True).stdout.strip()
        seconds = 0.0
        for part in out.replace('-', ':').split(':') if out else []:
            seconds = seconds * 60 + float(part)
        total += seconds
    return total


def flush_profile(console):
    """Counter deltas between the first and last `hydrogen-flush` line, or None without a profile kernel."""
    rows = [dict((k, int(v)) for k, v in re.findall(r'(\w+)=(\d+)', line))
            for line in console.splitlines() if line.startswith('hydrogen-flush')]
    if len(rows) < 2:
        return None
    return {k: rows[-1][k] - rows[0][k] for k in rows[-1]}


class Sites:
    """The form fixture and the Speedometer news site, served for the duration of a `with` block."""

    def __enter__(self):
        self.fixture = subprocess.Popen([sys.executable, ROOT / 'tests/interop/fixture.py'], stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, start_new_session=True)
        self.form = f'http://127.0.0.1:{int(self.fixture.stdout.readline())}'
        class Handler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *_):
                pass
        self.news_server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(
            Handler, directory=ROOT / '.work/dl/speedometer/src/resources/newssite/news-next/dist'))
        threading.Thread(target=self.news_server.serve_forever, daemon=True).start()
        self.news = f'http://127.0.0.1:{self.news_server.server_port}'
        return self

    def __exit__(self, *_):
        self.fixture.terminate()
        self.news_server.shutdown()


def measure(side, sites, home, iterations, image=m.NANOS_IMAGE, native_args=()):
    """Run the warm workload once on `side` ('native' or 'bctl') and return its rows and counters."""
    targs = argparse.Namespace(runtime='nanos', native_arg=list(native_args), native_binary=NATIVE_BINARY)
    trial = m.Trial(targs, home, side, resource_pass=False, image_path=image)
    trial.start()
    upstream = json.load(urllib.request.urlopen(trial.endpoint + '/json/version', timeout=5))['webSocketDebuggerUrl']
    log_path = trial.path / 'cdp.jsonl'
    relay = subprocess.Popen([VENV_PY, RELAY, upstream, log_path], stdout=subprocess.PIPE,
        stderr=open(trial.path / 'relay.log', 'wb'), start_new_session=True)
    env = trial.env | {'AGENT_BROWSER_CDP': relay.stdout.readline().decode().strip()}
    steps = []
    def timed(label, argv):
        start = time.monotonic_ns()
        output = m.invoke([m.AGENT, *argv], env=env)
        steps.append((label, start, time.monotonic_ns()))
        return output
    console = ''
    try:
        timed('cold open', ['open', sites.form + '/form'])
        timed('viewport', ['set', 'viewport', '1280', '720'])
        cpu_start = cpu_seconds(str(trial.path))
        for i in range(iterations):
            timed('noop eval', ['eval', '1'])
            timed('form open', ['open', sites.form + f'/form?warm{i}'])
            if 'Submit' not in timed('form snapshot', ['snapshot', '-i']):
                raise RuntimeError('form snapshot omitted Submit')
            timed('form fill', ['fill', '#name', 'Grace Hopper'])
            timed('form click', ['click', '#submit'])
            timed('form wait text', ['wait', '--text', 'sent Grace Hopper'])
            timed('form screenshot', ['screenshot', str(trial.path / 'form.png')])
            if 'sent Grace Hopper' not in timed('form get text', ['get', 'text', '#out']):
                raise RuntimeError('form did not submit')
            timed('news open', ['open', sites.news + f'/index.html?warm{i}#/business'])
            timed('news wait', ['wait', '#content-business-latest-trends'])
            timed('news snapshot', ['snapshot', '-i'])
            timed('news click', ['click', 'a[href="#/world"]'])
            timed('news wait world', ['wait', '#content-world-global-trends'])
            out = timed('news eval', ['--json', 'eval', "[...document.querySelectorAll('article')].length"])
            if json.loads(out)['data']['result'] < 10:
                raise RuntimeError('news extraction found too few articles')
            timed('news screenshot', ['screenshot', str(trial.path / 'news.png')])
        vm_cpu = cpu_seconds(str(trial.path)) - cpu_start
        for path in Path(trial.env.get('BCTL_HOME', '/nonexistent')).glob('machines/*/console.log'):
            console = path.read_text(errors='replace')
    finally:
        try:
            subprocess.run([m.AGENT, 'close'], env=env, capture_output=True, timeout=40)
            trial.close()
        finally:
            relay.terminate()
    time.sleep(0.2)
    intervals, methods = [], {}
    for line in log_path.read_text().splitlines():
        row = json.loads(line)
        intervals.append((row['start_ns'], row['end_ns']))
        methods.setdefault(row['method'], []).append((row['end_ns'] - row['start_ns']) / 1e6)
    rows = {}
    for label, lo, hi in steps:
        rows.setdefault(label, {'wall': [], 'browser': []})
        rows[label]['wall'].append((hi - lo) / 1e6)
        rows[label]['browser'].append(union_ms(intervals, lo, hi))
    for prefix, total in [('form ', 'FORM TOTAL'), ('news ', 'NEWS TOTAL')]:
        labels = [l for l in rows if l.startswith(prefix)]
        rows[total] = {key: [sum(rows[l][key][i] for l in labels) for i in range(iterations)] for key in ('wall', 'browser')}
    return {'rows': rows, 'methods': {k: statistics.median(v) for k, v in methods.items() if k in METHODS},
            'vm_cpu_seconds': vm_cpu, 'flush_profile': flush_profile(console)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--iterations', type=int, default=7)
    parser.add_argument('--sides', nargs='+', default=['native', 'bctl'])
    parser.add_argument('--bctl', type=Path, default=m.BCTL)
    parser.add_argument('--image', type=Path, default=m.NANOS_IMAGE)
    parser.add_argument('--native-arg', action='append', default=[])
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    m.BCTL = args.bctl.resolve()
    home = Path(tempfile.mkdtemp(prefix='wc-', dir=m.SCRATCH))
    results = {}
    with Sites() as sites:
        for side in args.sides:
            results[side] = measure(side, sites, home / side, args.iterations, args.image.resolve(), args.native_arg)['rows']
    args.out.write_text(json.dumps(results, indent=1))
    print(f'{"step":18}' + ''.join(f'{s + " wall":>13}{s + " cdp":>12}' for s in args.sides))
    for label in results[args.sides[0]]:
        print(f'{label:18}' + ''.join(f'{statistics.median(results[s][label]["wall"]):13.1f}'
            f'{statistics.median(results[s][label]["browser"]):12.1f}' for s in args.sides))


if __name__ == '__main__':
    main()
