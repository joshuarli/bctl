"""Browser-side time of the warm agent-browser workflows, measured at the CDP layer.

Both sides attach agent-browser with --cdp through cdp_relay.py. A step's browser-side time
is the union of CDP command intervals inside the step, so CLI spawn, daemon IPC and client
poll sleeps are excluded on both sides.
"""

import argparse
import functools
import http.server
import json
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path.cwd().resolve()
sys.path.insert(0, str(ROOT / 'tests/bench'))
import macos_workflows as m

VENV_PY = ROOT / '.work/interop/venv/bin/python'
RELAY = ROOT / 'tests/bench/cdp_relay.py'


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
    targs = argparse.Namespace(runtime='nanos', native_arg=args.native_arg,
        native_binary=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'))
    home = Path(tempfile.mkdtemp(prefix='wc-', dir=m.SCRATCH))
    fixture = subprocess.Popen([sys.executable, ROOT / 'tests/interop/fixture.py'], stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, start_new_session=True)
    form_port = int(fixture.stdout.readline())
    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass
    news = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(
        Handler, directory=ROOT / '.work/dl/speedometer/src/resources/newssite/news-next/dist'))
    threading.Thread(target=news.serve_forever, daemon=True).start()
    results = {}
    try:
        for side in args.sides:
            trial = m.Trial(targs, home / side, side, resource_pass=False, image_path=args.image)
            trial.start()
            upstream = json.load(urllib.request.urlopen(trial.endpoint + '/json/version', timeout=5))['webSocketDebuggerUrl']
            log_path = trial.path / 'cdp.jsonl'
            relay = subprocess.Popen([VENV_PY, RELAY, upstream, log_path], stdout=subprocess.PIPE,
                stderr=open(trial.path / 'relay.log', 'wb'), start_new_session=True)
            relay_url = relay.stdout.readline().decode().strip()
            env = trial.env | {'AGENT_BROWSER_CDP': relay_url}
            steps = []
            def timed(label, argv):
                start = time.monotonic_ns()
                output = m.invoke([m.AGENT, *argv], env=env)
                steps.append((label, start, time.monotonic_ns()))
                return output
            try:
                site = f'http://127.0.0.1:{form_port}'
                newsite = f'http://127.0.0.1:{news.server_port}'
                timed('cold open', ['open', site + '/form'])
                timed('viewport', ['set', 'viewport', '1280', '720'])
                for i in range(args.iterations):
                    timed('noop eval', ['eval', '1'])
                    timed('form open', ['open', site + f'/form?warm{i}'])
                    if 'Submit' not in timed('form snapshot', ['snapshot', '-i']):
                        raise RuntimeError('form snapshot omitted Submit')
                    timed('form fill', ['fill', '#name', 'Grace Hopper'])
                    timed('form click', ['click', '#submit'])
                    timed('form wait text', ['wait', '--text', 'sent Grace Hopper'])
                    timed('form screenshot', ['screenshot', str(trial.path / 'form.png')])
                    if 'sent Grace Hopper' not in timed('form get text', ['get', 'text', '#out']):
                        raise RuntimeError('form did not submit')
                    timed('news open', ['open', newsite + f'/index.html?warm{i}#/business'])
                    timed('news wait', ['wait', '#content-business-latest-trends'])
                    timed('news snapshot', ['snapshot', '-i'])
                    timed('news click', ['click', 'a[href="#/world"]'])
                    timed('news wait world', ['wait', '#content-world-global-trends'])
                    out = timed('news eval', ['--json', 'eval', "[...document.querySelectorAll('article')].length"])
                    if json.loads(out)['data']['result'] < 10:
                        raise RuntimeError('news extraction found too few articles')
                    timed('news screenshot', ['screenshot', str(trial.path / 'news.png')])
            finally:
                try:
                    subprocess.run([m.AGENT, 'close'], env=env, capture_output=True, timeout=40)
                    trial.close()
                finally:
                    relay.terminate()
            time.sleep(0.2)
            intervals = []
            methods = {}
            for line in log_path.read_text().splitlines():
                row = json.loads(line)
                intervals.append((row['start_ns'], row['end_ns']))
                methods.setdefault(row['method'], []).append((row['start_ns'], row['end_ns']))
            rows = {}
            for label, lo, hi in steps:
                rows.setdefault(label, {'wall': [], 'browser': []})
                rows[label]['wall'].append((hi - lo) / 1e6)
                rows[label]['browser'].append(union_ms(intervals, lo, hi))
            for prefix, total in [('form ', 'FORM TOTAL'), ('news ', 'NEWS TOTAL')]:
                labels = [l for l in rows if l.startswith(prefix)]
                for key in ('wall', 'browser'):
                    rows.setdefault(total, {'wall': [], 'browser': []})[key] = [
                        sum(rows[l][key][i] for l in labels) for i in range(args.iterations)]
            results[side] = rows
    finally:
        fixture.terminate()
        news.shutdown()
    args.out.write_text(json.dumps(results, indent=1))
    print(f'{"step":18}' + ''.join(f'{s + " wall":>13}{s + " cdp":>12}' for s in args.sides))
    for label in results[args.sides[0]]:
        print(f'{label:18}' + ''.join(f'{statistics.median(results[s][label]["wall"]):13.1f}'
            f'{statistics.median(results[s][label]["browser"]):12.1f}' for s in args.sides))


if __name__ == '__main__':
    main()
