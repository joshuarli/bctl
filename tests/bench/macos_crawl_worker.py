"""One fresh-browser crawl, artifact checks, and fresh native offline replay."""

import argparse
import contextlib
import hashlib
import json
from pathlib import Path
import sqlite3
import time

import crawl_site
import macos_workflows as workflows


def check(mirror, pages, browser):
    with contextlib.closing(sqlite3.connect((mirror / '_grab/state.db').as_uri() + '?mode=ro', uri=True)) as state:
        outputs = list(state.execute("select local_path from capture_manifest where kind='page' and status='visited'"))
        products = [row[0] for row in state.execute('select product from browser_sessions')]
        outcomes = list(state.execute("select outcome,count(*) from capture_attempts where kind='page' group by outcome"))
    if len(outputs) != pages or not products or any(product != browser for product in products):
        raise RuntimeError(f'incorrect capture count or browser: {len(outputs)}, {products}')
    content = []
    for index in range(pages):
        path = mirror / f'page-{index}.html'
        if path.is_dir():
            path /= 'index.html'
        text = path.read_text()
        marker = f'dynamic page {index} complete'
        if marker not in text or any(f'Page {index}, section {section}:' not in text for section in range(24)):
            raise RuntimeError(f'incomplete rendered content on page {index}')
        content.append(marker)
    saved = {}
    for path in (mirror / '_grab').rglob('*'):
        if path.suffix in {'.css', '.svg'} and path.is_file():
            saved.setdefault(path.name, []).append(path.read_bytes())
    for name, expected in [('base.css', crawl_site.CSS), ('dynamic.css', crawl_site.DYNAMIC_CSS),
                           ('image.svg', crawl_site.IMAGE), ('lazy.svg', crawl_site.IMAGE)]:
        if not saved.get(name) or any(value != expected for value in saved[name]):
            raise RuntimeError(f'missing or changed asset: {name}')
    return {'pages': pages, 'browser_products': products, 'page_outcomes': outcomes,
            'expected_markers_sha256': hashlib.sha256('\n'.join(content).encode()).hexdigest(), 'exact_assets': sorted(saved)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--path', type=Path, required=True)
    parser.add_argument('--side', choices=['native', 'bctl'], required=True)
    parser.add_argument('--url', required=True)
    parser.add_argument('--pages', type=int, required=True)
    parser.add_argument('--slots', type=int, default=4)
    parser.add_argument('--native-binary', type=Path, default=Path('/Applications/Chromium.app/Contents/MacOS/Chromium'))
    parser.add_argument('--native-arg', action='append', default=[])
    parser.add_argument('--shadowdriver', type=Path, default=workflows.ROOT.parent / 'shadowdriver')
    args = parser.parse_args()
    args.runtime = 'nanos'
    trial = workflows.Trial(args, args.path, args.side, resource_pass=False)
    receipt = {'status': 'running', 'side': args.side}
    try:
        started = time.perf_counter()
        receipt['startup_ms'] = trial.start()
        entry = 'import os,sys\nfrom grab.cli import main\nsys.exit(main(["--control-url",os.environ["BCTL_CDP_URL"],*sys.argv[1:]]))'
        captured = time.perf_counter()
        workflows.invoke([args.shadowdriver / 'grab/.venv/bin/python', '-c', entry, '-q', '-o', args.path / 'mirror',
            f'-j{args.slots}', '--browser-pages', str(args.slots), '--no-ublock', '--no-stealth', args.url], env=trial.env, timeout=900)
        mirrors = list((args.path / 'mirror').iterdir())
        if len(mirrors) != 1:
            raise RuntimeError('expected one site mirror')
        mirror = mirrors[0]
        receipt['capture'] = check(mirror, args.pages, trial.version)
        workflows.invoke([args.shadowdriver / 'grab/.venv/bin/grab', 'check', mirror], env=trial.env)
        receipt['capture_and_check_s'] = time.perf_counter() - captured
        verified = time.perf_counter()
        output = workflows.invoke([args.shadowdriver / 'grab/.venv/bin/grab', 'verify', mirror], env=trial.env, timeout=900)
        (args.path / 'offline-replay.txt').write_text(output)
        with contextlib.closing(sqlite3.connect((mirror / '_grab/state.db').as_uri() + '?mode=ro', uri=True)) as state:
            state.row_factory = sqlite3.Row
            receipt['verification'] = dict(state.execute('select * from verification_runs order by id desc limit 1').fetchone())
            receipt['browser_sessions'] = [dict(row) for row in state.execute('select * from browser_sessions')]
        report = receipt['verification']
        if report['pages_checked'] != args.pages or report['pages_passed'] != args.pages or report['pages_failed'] != 0:
            raise RuntimeError(f'offline replay did not verify every page: {report}')
        receipt['offline_replay_s'] = time.perf_counter() - verified
        receipt['total_s'] = time.perf_counter() - started
        receipt['status'] = 'pass'
    except BaseException as error:
        receipt['status'] = 'fail'
        receipt['error'] = str(error).replace(trial.endpoint, '<capability>') if trial.endpoint else str(error)
        raise
    finally:
        try:
            trial.close()
        except Exception as error:
            receipt['cleanup_error'] = str(error)
            if receipt['status'] != 'fail':
                receipt['status'] = 'fail'
                raise
        finally:
            workflows.save_results(args.path / 'result.json', receipt)


if __name__ == '__main__':
    with workflows.interruptions():
        main()
