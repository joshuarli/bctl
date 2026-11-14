import argparse
import asyncio
import base64
import gzip
import json
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

CATEGORIES = 'toplevel,disabled-by-default-toplevel.ipc,viz,cc,gpu,blink,devtools.timeline,latencyInfo,renderer.scheduler,sequence_manager'
OPERATIONS = ('form_click', 'form_screenshot', 'news_open', 'news_click', 'news_screenshot')
STAGES = ('cdp_dispatch', 'renderer_main', 'compositor_viz', 'copy_output_readback', 'image_encode', 'ipc_mojo', 'scheduler_toplevel')


def union(intervals):
    total = 0
    end = None
    for start, stop in sorted(intervals):
        if stop <= start:
            continue
        if end is None or start > end:
            total += stop - start
            end = stop
        elif stop > end:
            total += stop - end
            end = stop
    return total


def slices(events):
    processes, threads, stacks, result = {}, {}, {}, []
    for event in events:
        pid, tid = event.get('pid'), event.get('tid')
        if event.get('ph') == 'M':
            if event.get('name') == 'process_name':
                processes[pid] = event.get('args', {}).get('name', '')
            if event.get('name') == 'thread_name':
                threads[pid, tid] = event.get('args', {}).get('name', '')
        elif event.get('ph') == 'B':
            stacks.setdefault((pid, tid), []).append(event)
        elif event.get('ph') == 'E' and stacks.get((pid, tid)):
            begin = stacks[pid, tid].pop()
            item = dict(begin, dur=event['ts'] - begin['ts'])
            if 'tts' in begin and 'tts' in event:
                item['tdur'] = event['tts'] - begin['tts']
            result.append(item)
        elif event.get('ph') == 'X' and event.get('dur', 0) > 0:
            result.append(event)
    return result, processes, threads


def stage(event, thread):
    name = event.get('name', '').lower()
    if any(x in name for x in ('encodepng', 'pngcodec', 'pngencode', 'encode png', 'screenshotencode')):
        return 'image_encode'
    if any(x in name for x in ('copyoutput', 'readback', 'readpixels', 'bitmapcopy')):
        return 'copy_output_readback'
    if any(x in name for x in ('devtools', 'inspector', 'protocolmessage')):
        return 'cdp_dispatch'
    if any(x in name for x in ('mojo', 'connector::dispatch', 'interfaceendpointclient', 'ipc::')):
        return 'ipc_mojo'
    if any(x in name for x in ('compositorframe', 'surfaceaggregation', 'drawframe', 'raster', 'vizcompositor')) or thread == 'VizCompositorThread':
        return 'compositor_viz'
    if thread == 'CrRendererMain' and any(x in name for x in ('blink', 'v8', 'script', 'function', 'layout', 'style', 'paint', 'dom', 'render', 'evaluate')):
        return 'renderer_main'
    if any(x in name for x in ('threadcontrollerimpl::runtask', 'sequencemanager')):
        return 'scheduler_toplevel'
    return None


def summarize(items, processes, threads, windows):
    grouped = {}
    for event in items:
        name = stage(event, threads.get((event.get('pid'), event.get('tid')), ''))
        if not name:
            continue
        lo, hi = event['ts'], event['ts'] + event['dur']
        proc = processes.get(event.get('pid'), 'Unknown')
        key = (proc, name, event.get('pid'), event.get('tid'))
        for start, end in windows:
            a, b = max(lo, start), min(hi, end)
            if b > a:
                grouped.setdefault(key, []).append((a, b, event.get('tdur'), event['dur']))
    result = {}
    for (proc, name, pid, tid), spans in grouped.items():
        entry = result.setdefault(proc, {}).setdefault(name, {'union_ms': 0.0, 'cpu_ms': 0.0, 'cpu_available': False})
        entry['union_ms'] += union((a, b) for a, b, _, _ in spans) / 1000
        cpu_spans = [(a, b, cpu / dur) for a, b, cpu, dur in spans if cpu is not None and dur > 0]
        boundaries = sorted({x for a, b, _ in cpu_spans for x in (a, b)})
        entry['cpu_ms'] += sum((b-a) * max((ratio for x, y, ratio in cpu_spans if x < b and y > a), default=0)
                               for a, b in zip(boundaries, boundaries[1:])) / 1000
        entry['cpu_available'] |= bool(cpu_spans)
    return result


def stage_totals(items, threads, windows):
    grouped = {}
    for event in items:
        name = stage(event, threads.get((event.get('pid'), event.get('tid')), ''))
        if not name:
            continue
        lo, hi = event['ts'], event['ts'] + event['dur']
        for start, end in windows:
            a, b = max(lo, start), min(hi, end)
            if b > a:
                grouped.setdefault(name, []).append((a, b, event.get('tdur'), event['dur']))
    result = {}
    for name, spans in grouped.items():
        cpu_spans = [(a, b, cpu / dur) for a, b, cpu, dur in spans if cpu is not None and dur > 0]
        boundaries = sorted({x for a, b, _ in cpu_spans for x in (a, b)})
        cpu_ms = sum((b - a) * max((ratio for x, y, ratio in cpu_spans if x < b and y > a), default=0)
                     for a, b in zip(boundaries, boundaries[1:])) / 1000
        result[name] = {'union_ms': union((a, b) for a, b, _, _ in spans) / 1000,
                        'cpu_ms': cpu_ms, 'cpu_available': bool(cpu_spans)}
    return result


def analyze_operation_trace(trace_events, cdp_calls):
    items, processes, threads = slices(trace_events)
    intervals = [(e['ts'], e['ts'] + e['dur']) for e in items]
    windows = [(min(x[0] for x in intervals), max(x[1] for x in intervals))] if intervals else []
    browser_ms = union((row['start_ns'], row['end_ns']) for row in cdp_calls) / 1e6
    methods = {row['method'].lower() for row in cdp_calls}
    anchors = [e for e in items if (any(method in e.get('name', '').lower() for method in methods)
               or ('devtoolssession::dispatchprotocolmessage' in e.get('name', '').lower()
                   and any(method in json.dumps(e.get('args', {})).lower() for method in methods)))]
    aligned = [(e['ts'], e['ts'] + e['dur']) for e in anchors]
    attributed = union((e['ts'], e['ts'] + e['dur']) for e in items
                       if stage(e, threads.get((e.get('pid'), e.get('tid')), ''))) / 1000
    aligned_attributed = union((max(e['ts'], a), min(e['ts'] + e['dur'], b)) for e in items
                               if stage(e, threads.get((e.get('pid'), e.get('tid')), '')) for a, b in aligned) / 1000
    return {'trace_window': summarize(items, processes, threads, windows),
            'stage_totals': stage_totals(items, threads, windows),
            'cdp_command_aligned': summarize(items, processes, threads, aligned),
            'trace_window_ms': union(windows) / 1000, 'cdp_browser_ms': browser_ms,
            'attributed_stage_union_ms': attributed, 'aligned_attributed_stage_union_ms': aligned_attributed,
            'unattributed_vs_trace_window_ms': max(0.0, browser_ms - attributed),
            'unattributed_ms': max(0.0, browser_ms - aligned_attributed),
            'unscheduled_or_idle_ms': None,
            'visibility': 'Host CDP and trace clocks have different epochs; aligned totals use matching trace dispatch spans only. Unscheduled time is not observable.'}


def parse_stream(chunks):
    data = b''.join(base64.b64decode(chunk['data']) if chunk.get('base64Encoded') else chunk['data'].encode() for chunk in chunks)
    parsed = json.loads(data)
    if not isinstance(parsed, dict) or not isinstance(parsed.get('traceEvents'), list):
        raise ValueError('trace stream lacks traceEvents')
    return data, parsed['traceEvents']


async def trace_operation(url, operation):
    import websockets
    async with websockets.connect(url, max_size=None, ping_interval=None) as ws:
        async def send(id, method, params=None):
            await ws.send(json.dumps({'id': id, 'method': method, 'params': params or {}}))
        async def response(id):
            while True:
                msg = json.loads(await asyncio.wait_for(ws.recv(), 60))
                if msg.get('id') == id:
                    if 'error' in msg:
                        raise RuntimeError(str(msg['error']))
                    return msg.get('result', {})
        await send(1, 'Tracing.start', {'transferMode': 'ReturnAsStream', 'categories': CATEGORIES})
        await response(1)
        await asyncio.to_thread(operation)
        await send(2, 'Tracing.end')
        stream = None
        while stream is None:
            msg = json.loads(await asyncio.wait_for(ws.recv(), 60))
            if msg.get('id') == 2 and 'error' in msg:
                raise RuntimeError(str(msg['error']))
            if msg.get('method') == 'Tracing.tracingComplete':
                stream = msg['params']['stream']
        chunks = []
        while True:
            await send(3, 'IO.read', {'handle': stream})
            chunk = await response(3)
            chunks.append(chunk)
            if chunk.get('eof'):
                break
        await send(4, 'IO.close', {'handle': stream})
        await response(4)
        return parse_stream(chunks)


def run_attempt(side, sites, path, operations):
    args = argparse.Namespace(runtime='nanos', native_arg=['--disable-gpu'] if side == 'native-software' else [], native_binary=warm.NATIVE_BINARY)
    trial = m.Trial(args, path, 'native' if side == 'native-software' else side, resource_pass=False)
    relay = None
    try:
        trial.start()
        url = json.load(urllib.request.urlopen(trial.endpoint + '/json/version', timeout=5))['webSocketDebuggerUrl']
        log = path / 'cdp.jsonl'
        with (path / 'relay.log').open('wb') as err:
            relay = subprocess.Popen([warm.VENV_PY, warm.RELAY, url, log], stdout=subprocess.PIPE, stderr=err, start_new_session=True)
        env = trial.env | {'AGENT_BROWSER_CDP': relay.stdout.readline().decode().strip()}
        def run(*args):
            return m.invoke([m.AGENT, *args], env=env)
        run('open', sites.form + '/form')
        run('set', 'viewport', '1280', '720')
        run('snapshot', '-i')
        run('fill', '#name', 'Grace Hopper')
        run('click', '#submit')
        run('wait', '--text', 'sent Grace Hopper')
        run('screenshot', str(path / 'warm-form.png'))
        run('open', sites.news + '/index.html#/business')
        run('wait', '#content-business-latest-trends')
        run('snapshot', '-i')
        run('click', 'a[href="#/world"]')
        run('wait', '#content-world-global-trends')
        run('--json', 'eval', "[...document.querySelectorAll('article')].length")
        run('screenshot', str(path / 'warm-news.png'))
        rows = []
        for op in operations:
            if op.startswith('form_'):
                run('open', sites.form + '/form')
            elif op in ('news_click', 'news_screenshot'):
                run('open', sites.news + '/index.html#/business')
                run('wait', '#content-business-latest-trends')
            def action():
                nonlocal before
                before = len(log.read_text().splitlines()) if log.exists() else 0
                if op == 'form_click':
                    run('fill', '#name', 'Grace Hopper'); run('click', '#submit'); run('wait', '--text', 'sent Grace Hopper')
                elif op.endswith('screenshot'):
                    run('screenshot', str(path / (op + '.png')))
                elif op == 'news_open':
                    run('open', sites.news + '/index.html#/business'); run('wait', '#content-business-latest-trends')
                else:
                    run('click', 'a[href="#/world"]'); run('wait', '#content-world-global-trends')
            before = 0
            data, events = asyncio.run(trace_operation(url, action))
            calls = [json.loads(line) for line in log.read_text().splitlines()[before:]]
            artifact = path / (op + '.json.gz')
            with gzip.open(artifact, 'wb') as f:
                f.write(data)
            rows.append({'operation': op, 'trace': str(artifact), 'cdp_calls': calls, 'analysis': analyze_operation_trace(events, calls)})
        return rows
    finally:
        if relay:
            subprocess.run([m.AGENT, 'close'], env=env, capture_output=True, timeout=40)
            m.stop(relay)
        trial.close()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--sides', nargs='+', choices=['native', 'native-software', 'bctl'], default=['native', 'native-software', 'bctl'])
    p.add_argument('--operations', nargs='+', choices=OPERATIONS, default=OPERATIONS)
    p.add_argument('--rounds', type=int, default=1)
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    if args.rounds < 1:
        p.error('rounds must be positive')
    m.SCRATCH.mkdir(parents=True, exist_ok=True)
    home = args.out.parent / (args.out.stem + '-artifacts')
    home.mkdir(parents=True, exist_ok=False)
    receipt = {'instrumented_trace': True, 'attempts': []}
    with warm.Sites() as sites:
        for n in range(args.rounds):
            for side in args.sides:
                row = {'round': n, 'side': side, 'gate_checks': []}
                receipt['attempts'].append(row)
                try:
                    if not m.gate(row['gate_checks']):
                        row['status'] = 'deferred_host_busy'
                        continue
                    try:
                        row['operations'] = run_attempt(side, sites, home / f'{n}-{side}', args.operations)
                    finally:
                        clean = m.gate(row['gate_checks'], True)
                    row['status'] = 'pass' if clean else 'discarded_host_busy'
                finally:
                    m.save_results(args.out, receipt)


if __name__ == '__main__':
    main()
