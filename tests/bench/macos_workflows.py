"""Compare complete macOS automation workflows with native Chromium and bctl.

Uses the sibling Shadowdriver checkout and its existing Grab fixture. Latency
and resource passes are separate. Every attempt and load rejection is retained.
"""

from __future__ import annotations

import argparse
import contextlib
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import signal
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
SCRATCH = ROOT / '.work/tmp'
BCTL = ROOT / 'target/release/bctl'
AGENT = ROOT / '.work/interop/agent-browser/bin/agent-browser'
NANOS_IMAGE = ROOT / '.work/images/nanos-helium.img'
NANOS_RUNTIME = ROOT / '.work/runtime/nanos-krun'


def invoke(argv, *, env=None, timeout=120):
    process = subprocess.Popen([str(arg) for arg in argv], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except BaseException:
        stop(process)
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)
        raise
    if process.returncode:
        message = stderr[-1500:] or stdout[-1500:]
        if env and env.get('BCTL_CDP_URL'):
            message = message.replace(env['BCTL_CDP_URL'], '<capability>')
        raise RuntimeError(f'{Path(str(argv[0])).name} exited {process.returncode}: {message}')
    return stdout


@contextlib.contextmanager
def interruptions():
    class Interrupted(BaseException):
        def __init__(self, signum):
            self.signum = signum
    def interrupt(signum, _):
        raise Interrupted(signum)
    signals = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT)
    previous = {signum: signal.signal(signum, interrupt) for signum in signals}
    try:
        yield
    except Interrupted as error:
        signal.signal(error.signum, signal.SIG_DFL)
        os.kill(os.getpid(), error.signum)
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def stop(process):
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def tree(roots, rows):
    owned = set(roots)
    for _ in range(len(rows)):
        children = {pid for pid, ppid, _ in rows if ppid in owned}
        if children <= owned:
            break
        owned |= children
    return owned


def process_table():
    output = subprocess.check_output(['ps', '-Ao', 'pid=,ppid=,command='], timeout=10)
    return os.fsdecode(output)


def save_results(path, results):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, scratch = tempfile.mkstemp(prefix=f'.{path.name}.', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(results, stream, indent=2)
            stream.write('\n')
        os.replace(scratch, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(scratch)


def physical_footprint_sample(output, expected):
    values = {int(pid): int(value.replace(',', '')) for pid, value in re.findall(
        r'^.*\[(\d+)\]:[^\n]*Footprint:\s+([\d,]+) B', output, re.MULTILINE)}
    if set(values) != expected:
        raise ValueError('process set changed during footprint collection')
    summaries = re.findall(r'^Summary Footprint:\s+([\d,]+) B', output, re.MULTILINE)
    if len(expected) == 1 and not summaries:
        total = next(iter(values.values()))
    elif len(summaries) == 1:
        total = int(summaries[0].replace(',', ''))
    else:
        raise ValueError('missing unique aggregate physical footprint')
    if not max(values.values()) <= total <= sum(values.values()):
        raise ValueError('aggregate physical footprint is outside process bounds')
    return {'physical_bytes': total, 'processes': values}


class Resources:
    def __init__(self, trial, *, interval_s=0.25):
        self.trial = trial
        self.interval_s = interval_s
        self.done = threading.Event()
        self.samples = []
        self.errors = []
        self.thread = threading.Thread(target=self.run)

    def run(self):
        while not self.done.is_set():
            try:
                table = process_table()
                rows = []
                for line in table.splitlines():
                    pid, ppid, command = line.strip().split(None, 2)
                    if Path(command.split()[0]).name not in {'ps', 'footprint', 'lsof'}:
                        rows.append((int(pid), int(ppid), command))
                roots = {os.getpid(), *self.trial.roots}
                roots.update(pid for pid, _, command in rows if str(self.trial.path) in command)
                for pidfile in self.trial.socket_dir.rglob('*.pid'):
                    with contextlib.suppress(OSError, ValueError):
                        roots.add(int(pidfile.read_text().strip()))
                pids = tree(roots, rows) & {row[0] for row in rows}
                args = ['footprint', '--noCategories', '-f', 'bytes']
                for pid in sorted(pids):
                    args += ['-p', str(pid)]
                output = invoke(args, timeout=20)
                sample = physical_footprint_sample(output, pids)
                self.samples.append({'elapsed_s': time.monotonic() - self.started, **sample})
                if sample['physical_bytes'] == max(row['physical_bytes'] for row in self.samples):
                    (self.trial.path / 'peak-footprint.txt').write_text(output)
            except Exception as error:
                self.errors.append(str(error))
            self.done.wait(self.interval_s)

    def __enter__(self):
        self.started = time.monotonic()
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.done.set()
        self.thread.join(timeout=25)
        if self.thread.is_alive():
            raise RuntimeError('resource sampler did not stop')


class Trial:
    def __init__(self, args, path, side, *, resource_pass, image_path=NANOS_IMAGE, runtime_path=NANOS_RUNTIME):
        self.args, self.path, self.side = args, path, side
        self.resource_pass = resource_pass
        self.roots = set()
        self.browser = None
        self.endpoint = None
        self.agent_started = False
        self.vm_started = False
        self.path.mkdir(parents=True)
        tag = hashlib.sha256(str(path).encode()).hexdigest()[:6]
        self.socket_dir = path.parent / f's-{tag}'
        self.env = {key: value for key, value in os.environ.items() if not key.startswith(('AGENT_BROWSER_', 'BCTL_'))}
        self.env |= {'TMPDIR': str(SCRATCH), 'BCTL_HOME': str(path / 'bctl'), 'BCTL_RUNTIME': args.runtime,
                     'AGENT_BROWSER_SOCKET_DIR': str(self.socket_dir), 'AGENT_BROWSER_SESSION': f'bm{os.getpid()}-{tag}',
                     'AGENT_BROWSER_NO_AUTO_DIALOG': '1', 'AGENT_BROWSER_EXECUTABLE_PATH': str(args.native_binary)}
        self.socket_dir.mkdir(mode=0o700)
        (path / 'empty-config.json').write_text('{}\n')
        self.env['AGENT_BROWSER_CONFIG'] = str(path / 'empty-config.json')
        home = path / 'bctl'
        (home / 'runtime').mkdir(parents=True)
        (home / 'images').mkdir()
        (home / 'runtime/nanos').symlink_to(runtime_path.resolve())
        for name, target in [('nanos-helium.img', image_path), ('nanos-helium.profile.img', image_path.with_suffix('.profile.img'))]:
            (home / 'images' / name).symlink_to(target.resolve())
        self.image = 'nanos-helium'
        if args.native_arg:
            self.env['AGENT_BROWSER_ARGS'] = ','.join(args.native_arg)

    def discover_daemon(self):
        lock = self.path / 'bctl/daemon.lock'
        if lock.exists():
            done = subprocess.run(['lsof', '-t', str(lock)], capture_output=True, text=True)
            self.roots.update(int(pid) for pid in done.stdout.split())

    def start(self):
        started = time.perf_counter()
        if self.side == 'native':
            profile = self.path / 'profile'
            profile.mkdir()
            log = (self.path / 'native.log').open('wb')
            self.browser = subprocess.Popen([str(self.args.native_binary), '--headless=new', '--no-first-run',
                '--no-default-browser-check', '--disable-background-networking', '--remote-debugging-port=0',
                f'--user-data-dir={profile}', *self.args.native_arg, 'about:blank'], stdout=log, stderr=log, start_new_session=True)
            log.close()
            self.roots.add(self.browser.pid)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if self.browser.poll() is not None:
                    raise RuntimeError('native Chromium exited before readiness')
                try:
                    lines = (profile / 'DevToolsActivePort').read_text().splitlines()
                    if not lines:
                        time.sleep(0.02)
                        continue
                    port = int(lines[0])
                    self.endpoint = f'http://127.0.0.1:{port}'
                    self.version = json.load(urllib.request.urlopen(self.endpoint + '/json/version', timeout=1))['Browser']
                    break
                except (OSError, ValueError):
                    time.sleep(0.02)
            else:
                raise RuntimeError('native Chromium readiness timed out')
        else:
            self.vm_started = True
            invoke([BCTL, 'up', 'bench', '--image', self.image, '--allow-private', '127.0.0.1/32'], env=self.env)
            self.endpoint = invoke([BCTL, 'cdp-url', 'bench'], env=self.env).strip()
            self.version = json.load(urllib.request.urlopen(self.endpoint + '/json/version', timeout=5))['Browser']
            if self.resource_pass:
                self.discover_daemon()
        self.env['BCTL_CDP_URL'] = self.endpoint
        return (time.perf_counter() - started) * 1000

    def site(self, port):
        return f'http://127.0.0.1:{port}'

    def close(self):
        errors = []
        if self.agent_started:
            try:
                invoke([AGENT, 'close'], env=self.env, timeout=40)
            except Exception as error:
                errors.append(str(error))
        if self.browser is not None:
            stop(self.browser)
        if self.vm_started:
            session = f'ab-{self.env["AGENT_BROWSER_SESSION"]}' if self.agent_started else 'bench'
            try:
                sessions = json.loads(invoke([BCTL, 'status', '--json'], env=self.env))['sessions']
                if any(row['name'] == session for row in sessions):
                    invoke([BCTL, 'down', session], env=self.env, timeout=45)
            except Exception as error:
                raise RuntimeError(f'VM cleanup failed in {self.path}: {error}; earlier cleanup errors: {errors}') from error
        self.discover_daemon()
        for pid in self.roots:
            if self.browser is not None and pid == self.browser.pid:
                continue
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM)
        if errors:
            raise RuntimeError('; '.join(errors))


def png(path):
    data = path.read_bytes()
    if data[:8] != b'\x89PNG\r\n\x1a\n' or len(data) < 100:
        raise RuntimeError('screenshot is not a nonempty PNG')
    return len(data)


def shadowdriver(trial, port):
    env = trial.env | {'SHADOWDRIVER_MODULE': str(trial.args.shadowdriver / 'ts/dist/mod.js'),
        'WORKFLOW_SITE': trial.site(port), 'WORKFLOW_SCREENSHOT': str(trial.path / 'shadowdriver.png'),
        'WORKFLOW_PROFILE': str(trial.path / 'sdk-profile')}
    result = json.loads(invoke(['node', ROOT / 'tests/bench/shadowdriver_workflow.mjs'], env=env))
    if result['submitted'] != 'Ada Lovelace' or not result['ax_textbox'] or result['product'] != trial.version:
        raise RuntimeError('Shadowdriver did not complete the form workflow')
    png(trial.path / 'shadowdriver.png')
    return result


def grab(trial, port):
    out = trial.path / 'mirror'
    entry = 'import os, sys\nfrom grab.cli import main\nsys.exit(main(["--control-url", os.environ["BCTL_CDP_URL"], *sys.argv[1:]]))'
    invoke([trial.args.shadowdriver / 'grab/.venv/bin/python', '-c', entry, '-q', '-o', out, '-j4', '--browser-pages', '4',
        '--no-ublock', '--no-stealth', trial.site(port) + '/index.html'], env=trial.env, timeout=180)
    mirrors = list(out.iterdir())
    if len(mirrors) != 1:
        raise RuntimeError('Grab did not produce one site mirror')
    mirror = mirrors[0]
    expected = {'index.html', 'alpha.html', 'beta.html', 'gamma.html'}
    if not expected <= {path.name for path in mirror.glob('*.html')}:
        raise RuntimeError('Grab omitted expected pages')
    for name in ('alpha', 'beta', 'gamma'):
        path = mirror / f'{name}.html'
        if path.is_dir():
            path = path / 'index.html'
        if f'data-marker="{name}"' not in path.read_text():
            raise RuntimeError(f'Grab omitted the {name} page content')
    stylesheets = list((mirror / '_grab').glob('*/shared.css'))
    source = trial.args.shadowdriver / 'grab/tests/fixtures/capture-concurrency/shared.css'
    if len(stylesheets) != 1 or stylesheets[0].read_bytes() != source.read_bytes():
        raise RuntimeError('Grab did not persist the complete shared stylesheet')
    if not (mirror / '_grab/state.db').is_file():
        raise RuntimeError('Grab did not persist its state')
    with contextlib.closing(sqlite3.connect((mirror / '_grab/state.db').as_uri() + '?mode=ro', uri=True)) as state:
        products = [row[0] for row in state.execute('select product from browser_sessions')]
        if not products or any(product != trial.version for product in products):
            raise RuntimeError('Grab used a different browser from the requested endpoint')
        outcomes = [dict(zip(('kind', 'outcome', 'failure_kind', 'failure_reason'), row)) for row in state.execute(
            'select kind,outcome,failure_kind,failure_reason from capture_attempts')]
        if sum(row['kind'] == 'page' and row['outcome'] == 'success' for row in outcomes) != 4:
            raise RuntimeError('Grab did not record four successful page captures')
    invoke([trial.args.shadowdriver / 'grab/.venv/bin/grab', 'check', mirror], env=trial.env)
    return {'pages': 4, 'mirror_checked': True, 'content_checked': True, 'browser_products': products,
            'files': sum(path.is_file() for path in mirror.rglob('*')), 'capture_outcomes': outcomes}


def agent_browser(trial, port, *, news=False):
    if trial.side == 'bctl':
        trial.env.pop('AGENT_BROWSER_ARGS', None)
        config = [{'name': 'bctl', 'command': str(BCTL), 'args': ['agent-browser-plugin', '--image', trial.image,
            '--allow-private', '127.0.0.1/32'], 'capabilities': ['browser.provider']}]
        trial.env['AGENT_BROWSER_PLUGINS'] = json.dumps(config)
        trial.env['AGENT_BROWSER_PROVIDER'] = 'bctl'
        trial.vm_started = True
    url = trial.site(port) + ('/index.html#/business' if news else '/form')
    start = time.perf_counter()
    trial.agent_started = True
    invoke([AGENT, 'open', url], env=trial.env)
    cold_ms = (time.perf_counter() - start) * 1000
    if trial.resource_pass:
        trial.discover_daemon()
    invoke([AGENT, 'set', 'viewport', '1280', '720'], env=trial.env)
    start = time.perf_counter()
    if news:
        invoke([AGENT, 'open', trial.site(port) + '/index.html?warm#/business'], env=trial.env)
        invoke([AGENT, 'wait', '#content-business-latest-trends'], env=trial.env)
        snapshot = invoke([AGENT, 'snapshot', '-i'], env=trial.env)
        if 'World' not in snapshot or 'Business' not in snapshot:
            raise RuntimeError('agent-browser snapshot omitted news navigation')
        invoke([AGENT, 'click', 'a[href="#/world"]'], env=trial.env)
        invoke([AGENT, 'wait', '#content-world-global-trends'], env=trial.env)
        expression = '''({hash: location.hash, articles: [...document.querySelectorAll('article')].map(a => ({
            heading: a.querySelector('h2,h3')?.textContent ?? '', text: a.textContent, image: a.querySelector('img')?.getAttribute('src') ?? ''}))})'''
        response = json.loads(invoke([AGENT, '--json', 'eval', expression], env=trial.env))
        result = response['data']['result']
        if result['hash'] != '#/world' or len(result['articles']) < 10 or not {'Africa', 'China', 'Russia'} <= {
            row['heading'] for row in result['articles']} or sum(len(row['text']) for row in result['articles']) < 10000:
            raise RuntimeError('agent-browser did not extract the rendered world articles')
        (trial.path / 'articles.json').write_text(json.dumps(result, indent=2) + '\n')
        invoke([AGENT, 'screenshot', trial.path / 'agent-browser.png'], env=trial.env)
        return {'cold_open_ms': cold_ms, 'warm_workflow_ms': (time.perf_counter() - start) * 1000,
                'screenshot_bytes': png(trial.path / 'agent-browser.png'), 'articles': len(result['articles']),
                'content_sha256': hashlib.sha256(json.dumps(result, sort_keys=True).encode()).hexdigest()}
    invoke([AGENT, 'open', trial.site(port) + '/form?warm'], env=trial.env)
    snapshot = invoke([AGENT, 'snapshot', '-i'], env=trial.env)
    if 'Name' not in snapshot or 'Submit' not in snapshot:
        raise RuntimeError('agent-browser snapshot omitted the form')
    invoke([AGENT, 'fill', '#name', 'Grace Hopper'], env=trial.env)
    invoke([AGENT, 'click', '#submit'], env=trial.env)
    invoke([AGENT, 'wait', '--text', 'sent Grace Hopper'], env=trial.env)
    invoke([AGENT, 'screenshot', trial.path / 'agent-browser.png'], env=trial.env)
    if 'sent Grace Hopper' not in invoke([AGENT, 'get', 'text', '#out'], env=trial.env):
        raise RuntimeError('agent-browser form did not submit')
    return {'cold_open_ms': cold_ms, 'warm_workflow_ms': (time.perf_counter() - start) * 1000,
            'screenshot_bytes': png(trial.path / 'agent-browser.png')}


def gate(checks, after=False):
    done = subprocess.run(['perf-gate-ready', *(['-1'] if after else [])], capture_output=True, text=True)
    checks.append({'after': after, 'code': done.returncode, 'load': invoke(['sysctl', '-n', 'vm.loadavg']).strip(),
                   'diagnostic': done.stderr.splitlines()[0] if done.stderr else ''})
    return done.returncode == 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native-binary', type=Path, default=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'))
    parser.add_argument('--shadowdriver', type=Path, default=ROOT.parent / 'shadowdriver')
    parser.add_argument('--runtime', choices=['nanos'], default='nanos')
    parser.add_argument('--bctl-image', type=Path, default=NANOS_IMAGE, help='image file with a matching .profile.img template')
    parser.add_argument('--bctl-runtime-dir', type=Path, default=NANOS_RUNTIME)
    parser.add_argument('--native-arg', action='append', default=[], help='extra native launch argument, including agent-browser; use --native-arg=--flag')
    parser.add_argument('--rounds', type=int, default=5)
    parser.add_argument('--resource-rounds', type=int, default=5)
    parser.add_argument('--cpu-rounds', type=int, default=0)
    parser.add_argument('--workloads', nargs='+', choices=['shadowdriver', 'grab', 'agent-browser', 'agent-browser-news'],
                        default=['shadowdriver', 'grab', 'agent-browser', 'agent-browser-news'])
    parser.add_argument('--quiet-wait', type=float, default=120)
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    if sys.platform != 'darwin' or min(args.rounds, args.resource_rounds, args.cpu_rounds) < 0 or args.rounds + args.resource_rounds + args.cpu_rounds == 0:
        parser.error('requires macOS, nonnegative round counts, and at least one pass')
    stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
    SCRATCH.mkdir(parents=True, exist_ok=True)
    home = Path(tempfile.mkdtemp(prefix='mw-', dir=SCRATCH))
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    node = Path(shutil.which('node') or parser.error('node is required'))
    node_version = invoke([node, '--version']).strip()
    requirement = json.loads((args.shadowdriver / 'ts/package.json').read_text())['engines']['node']
    minimum = re.fullmatch(r'>=(\d+)\.(\d+)\.(\d+)', requirement)
    if minimum is None or tuple(int(part) for part in node_version.lstrip('v').split('.')) < tuple(map(int, minimum.groups())):
        parser.error(f'Shadowdriver requires Node.js {requirement} on PATH')
    runtime_files = [args.bctl_image.resolve(), args.bctl_image.resolve().with_suffix('.profile.img'),
                     args.bctl_runtime_dir.resolve() / 'nanos-krun', args.bctl_runtime_dir.resolve() / 'lib/libkrun.dylib',
                     args.bctl_runtime_dir.resolve() / 'kernel.img']
    results = {'started': stamp, 'completed': False, 'runtime': args.runtime, 'native_binary': str(args.native_binary),
        'native_version': invoke([args.native_binary, '--version']).strip(), 'bctl_commit': invoke(['git', '-C', ROOT, 'rev-parse', 'HEAD']).strip(),
        'shadowdriver_commit': invoke(['git', '-C', args.shadowdriver, 'rev-parse', 'HEAD']).strip(), 'attempts': [], 'summary': {},
        'bctl_dirty': bool(invoke(['git', '-C', ROOT, 'status', '--porcelain']).strip()),
        'shadowdriver_dirty': bool(invoke(['git', '-C', args.shadowdriver, 'status', '--porcelain']).strip()),
        'node_version': node_version, 'agent_browser_version': invoke([AGENT, '--version']).strip(),
        'native_extra_args': args.native_arg, 'cpu_rounds': args.cpu_rounds, 'bctl_image': str(args.bctl_image.resolve()),
        'bctl_runtime_dir': str(args.bctl_runtime_dir.resolve()),
        'balloon_reclaim': os.environ.get('SMOLVM_BALLOON_RECLAIM'),
        'cpu_method': 'separate fresh-profile passes; libproc RUSAGE_INFO_V1 live processes plus reaped-child CPU; Mach timebase conversion; includes driver, fixtures, clients, daemon, supervisor and VMM or native browser tree, plus process discovery; excludes cleanup; snapshots are not atomic',
        'vm_configuration': 'bctl production defaults, actual resources recorded on resource attempts',
        'artifacts_sha256': {str(path): digest(path) for path in [Path(__file__), ROOT / 'tests/bench/shadowdriver_workflow.mjs',
            BCTL, AGENT, node, args.native_binary, args.shadowdriver / 'ts/dist/src/browser.js', args.shadowdriver / 'py/python/shadowdriver/_native.abi3.so', *runtime_files]},
        'host': {'model': invoke(['sysctl', '-n', 'hw.model']).strip(), 'cpus': int(invoke(['sysctl', '-n', 'hw.ncpu'])),
                 'memory_bytes': int(invoke(['sysctl', '-n', 'hw.memsize'])), 'macos': invoke(['sw_vers', '-productVersion']).strip()},
        'method': 'latency and physical-footprint sampling use separate fresh-profile passes; alternating sides; OS and image caches warm; default VM resources; native unrestricted CPU',
        'resource_method': 'deduplicated Summary Footprint of driver, fixtures, clients, daemon, supervisor, VMM or native browser tree; includes compressed pages; 250 ms best-effort sampling; not an exact peak',
        'fixture_transport': 'identical IPv4 loopback URL for host HTTP, native browser, and guest browser; guest proxy explicitly permits only 127.0.0.1/32'}
    out = args.out or home / 'results.json'
    if args.cpu_rounds:
        results['artifacts_sha256'][str(ROOT / 'tests/bench/macos_cpu.py')] = digest(ROOT / 'tests/bench/macos_cpu.py')
    fixture_log = (home / 'fixture.log').open('wb')
    fixture = subprocess.Popen([sys.executable, ROOT / 'tests/interop/fixture.py'], stdout=subprocess.PIPE, stderr=fixture_log, start_new_session=True)
    form_port = int(fixture.stdout.readline())
    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass
    class Server(http.server.ThreadingHTTPServer):
        request_queue_size = 128
    handler = functools.partial(Handler, directory=args.shadowdriver / 'grab/tests/fixtures/capture-concurrency')
    server = Server(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    news_server = None
    news_thread = None
    if 'agent-browser-news' in args.workloads:
        news_root = ROOT / '.work/dl/speedometer/src'
        results['speedometer_commit'] = invoke(['git', '-C', news_root, 'rev-parse', 'HEAD']).strip()
        results['speedometer_dirty'] = bool(invoke(['git', '-C', news_root, 'status', '--porcelain']).strip())
        news_server = Server(('127.0.0.1', 0), functools.partial(
            Handler, directory=news_root / 'resources/newssite/news-next/dist'))
        news_thread = threading.Thread(target=news_server.serve_forever)
        news_thread.start()
    deadline = time.monotonic() + args.quiet_wait
    try:
        for phase, rounds in [('latency', args.rounds), ('resources', args.resource_rounds), ('cpu', args.cpu_rounds)]:
            for round in range(rounds):
                for side in (['native', 'bctl'] if round % 2 == 0 else ['bctl', 'native']):
                    for workload_id, workload in enumerate(args.workloads):
                        attempt = {'phase': phase, 'round': round, 'side': side, 'workload': workload, 'gate_checks': []}
                        results['attempts'].append(attempt)
                        path = home / f'{phase[0]}{round}{side[0]}{workload_id}'
                        attempt['artifacts'] = str(path.relative_to(ROOT))
                        trial = Trial(args, path, side, resource_pass=phase == 'resources', image_path=args.bctl_image,
                                      runtime_path=args.bctl_runtime_dir)
                        try:
                            timed = phase in {'latency', 'cpu'}
                            quiet = gate(attempt['gate_checks']) if timed else False
                            while timed and not quiet and time.monotonic() < deadline:
                                time.sleep(1)
                                quiet = gate(attempt['gate_checks'])
                            if timed and not quiet:
                                attempt['status'] = 'deferred'
                                attempt['timing_status'] = 'deferred_host_busy'
                                continue
                            print(f'{phase} {round} {side} {workload}', flush=True)
                            sampler = Resources(trial) if phase == 'resources' else contextlib.nullcontext()
                            with sampler:
                                if phase == 'cpu':
                                    from macos_cpu import trial_snapshot
                                    attempt['cpu_before'] = trial_snapshot(trial)
                                started = time.perf_counter()
                                if not workload.startswith('agent-browser'):
                                    attempt['startup_ms'] = trial.start()
                                    attempt['browser_version'] = trial.version
                                task_start = time.perf_counter()
                                attempt['result'] = {'shadowdriver': lambda: shadowdriver(trial, form_port),
                                    'grab': lambda: grab(trial, server.server_port), 'agent-browser': lambda: agent_browser(trial, form_port),
                                    'agent-browser-news': lambda: agent_browser(trial, news_server.server_port, news=True)}[workload]()
                                attempt['task_ms'] = (time.perf_counter() - task_start) * 1000
                                attempt['total_ms'] = (time.perf_counter() - started) * 1000
                                if phase == 'cpu':
                                    attempt['cpu_after'] = trial_snapshot(trial)
                                    attempt['cpu_s'] = attempt['cpu_after']['cpu_s'] - attempt['cpu_before']['cpu_s']
                                    if attempt['cpu_s'] <= 0:
                                        raise RuntimeError('nonpositive CPU cost')
                                if phase == 'resources':
                                    time.sleep(1)
                            attempt['status'] = 'pass'
                            attempt['timing_status'] = ('clean' if quiet and gate(attempt['gate_checks'], True) else 'discarded_host_busy') if timed else f'{phase}_pass'
                            if workload.startswith('agent-browser'):
                                attempt['agent_user_agent'] = invoke([AGENT, 'eval', 'navigator.userAgent'], env=trial.env).strip()
                            if phase == 'resources':
                                attempt['resource_samples'] = sampler.samples
                                attempt['resource_errors'] = sampler.errors
                                if not sampler.samples:
                                    raise RuntimeError('no complete physical-footprint sample')
                                attempt['sampled_peak_physical_bytes'] = max(sample['physical_bytes'] for sample in sampler.samples)
                                if side == 'bctl':
                                    session = f'ab-{trial.env["AGENT_BROWSER_SESSION"]}' if workload.startswith('agent-browser') else 'bench'
                                    inspected = json.loads(invoke([BCTL, 'inspect', session, '--json'], env=trial.env))
                                    attempt['runtime_resources'] = inspected['sessions'][0]['resources']
                        except Exception as error:
                            attempt['status'] = 'fail'
                            attempt['error'] = str(error).replace(trial.endpoint, '<capability>') if trial.endpoint else str(error)
                            print(f'FAIL: {attempt["error"]}', flush=True)
                        finally:
                            try:
                                trial.close()
                            except Exception as error:
                                attempt['status'] = 'fail'
                                attempt['cleanup_error'] = str(error)
                            save_results(out, results)
                        if attempt['status'] == 'fail':
                            raise RuntimeError(f'{phase}/{side}/{workload} failed; see {out}')
        results['shadowdriver_final_commit'] = invoke(['git', '-C', args.shadowdriver, 'rev-parse', 'HEAD']).strip()
        results['changed_artifacts'] = [str(path) for path, before in results['artifacts_sha256'].items() if digest(Path(path)) != before]
        if results['shadowdriver_final_commit'] != results['shadowdriver_commit'] or results['changed_artifacts']:
            raise RuntimeError('benchmark inputs changed during measurement; comparison is invalid')
        for workload in args.workloads:
            results['summary'][workload] = {}
            for side in ['native', 'bctl']:
                rows = [row for row in results['attempts'] if row['workload'] == workload and row['side'] == side and row['status'] == 'pass']
                values = [row['total_ms'] for row in rows if row['phase'] == 'latency' and row['timing_status'] == 'clean']
                results['summary'][workload][side] = {'clean_latency_samples': len(values),
                    'median_total_ms': statistics.median(values) if len(values) >= 5 else None,
                    'range_total_ms': [min(values), max(values)] if values else None,
                    'failures': sum(row['status'] == 'fail' for row in results['attempts'] if row['workload'] == workload and row['side'] == side)}
                metrics = {}
                for name, field in [('total_ms', 'total_ms'), ('task_ms', 'task_ms'), ('startup_ms', 'startup_ms'),
                                    ('agent_cold_open_ms', 'cold_open_ms'), ('agent_warm_workflow_ms', 'warm_workflow_ms')]:
                    samples = [row.get(field, row['result'].get(field)) for row in rows if row['phase'] == 'latency' and row['timing_status'] == 'clean']
                    samples = [value for value in samples if value is not None]
                    if samples:
                        metrics[name] = {'n': len(samples), 'median': statistics.median(samples) if len(samples) >= 5 else None,
                                         'min': min(samples), 'max': max(samples)}
                memory = [row['sampled_peak_physical_bytes'] for row in rows if row['phase'] == 'resources']
                if memory:
                    metrics['sampled_peak_physical_bytes'] = {'n': len(memory), 'median': statistics.median(memory) if len(memory) >= 5 else None,
                                                             'min': min(memory), 'max': max(memory)}
                cpu = [row['cpu_s'] for row in rows if row['phase'] == 'cpu' and row['timing_status'] == 'clean']
                if cpu:
                    metrics['cpu_s'] = {'n': len(cpu), 'median': statistics.median(cpu) if len(cpu) >= 5 else None,
                                        'min': min(cpu), 'max': max(cpu)}
                results['summary'][workload][side]['metrics'] = metrics
        news_hashes = {row['result']['content_sha256'] for row in results['attempts']
                       if row['workload'] == 'agent-browser-news' and row['status'] == 'pass'}
        if len(news_hashes) > 1:
            raise RuntimeError('native and bctl news extraction content differed')
        results['completed'] = True
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        if news_server is not None:
            news_server.shutdown()
            news_server.server_close()
            news_thread.join(timeout=5)
        stop(fixture)
        fixture_log.close()
        save_results(out, results)
    print(out, flush=True)
    return int(any(row['status'] == 'fail' for row in results['attempts']))


if __name__ == '__main__':
    with interruptions():
        sys.exit(main())
