import argparse
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from macos_workflows import SCRATCH, Trial, physical_footprint_sample, process_table, save_results

FAKE_BROWSER = '''import http.server, json, sys, threading, time
profile = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('--user-data-dir='))
port_file = profile + '/DevToolsActivePort'
open(port_file, 'w').close()
time.sleep(0.3)
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({'Browser': 'Fake/1'}).encode()
        self.send_response(200); self.end_headers(); self.wfile.write(body)
    def log_message(self, *_):
        pass
server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
with open(port_file, 'w') as stream:
    stream.write(f'{server.server_port}\\n/devtools/browser/fake\\n')
server.serve_forever()
'''


class ReceiptTests(unittest.TestCase):
    def test_an_incomplete_update_preserves_the_previous_receipt(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH, prefix='receipt-') as directory:
            path = Path(directory) / 'results.json'
            save_results(path, {'completed': False})
            before = path.read_bytes()
            with self.assertRaises(TypeError):
                save_results(path, {'completed': True, 'invalid': object()})
            self.assertEqual(path.read_bytes(), before)
            self.assertEqual(list(Path(directory).iterdir()), [path])


@unittest.skipUnless(sys.platform == 'darwin', 'requires macOS process arguments')
class ProcessTableTests(unittest.TestCase):
    def test_foreign_non_utf8_arguments_do_not_stop_memory_collection(self):
        foreign = subprocess.Popen([os.fsencode(sys.executable), b'-c',
            b'import signal; print("ready", flush=True); signal.pause()', b'\xff'], stdout=subprocess.PIPE)
        try:
            self.assertEqual(foreign.stdout.readline(), b'ready\n')
            table = process_table()
            line = next(line for line in table.splitlines() if line.strip().split()[0] == str(foreign.pid))
            self.assertIn(b'\xff', os.fsencode(line))
        finally:
            foreign.terminate()
            try:
                foreign.wait(timeout=5)
            except subprocess.TimeoutExpired:
                foreign.kill()
                foreign.wait(timeout=5)
            foreign.stdout.close()


class PhysicalFootprintTests(unittest.TestCase):
    def test_multiple_processes_do_not_count_the_summary_twice(self):
        output = '''Python [65302]: 64-bit    Footprint: 8,274,424 B (16384 bytes per page)
Auxiliary data:
    phys_footprint: 8274424 B
    phys_footprint_peak: 8274424 B
sleep [65303]: 64-bit    Footprint: 1278192 B (16384 bytes per page)
Summary Footprint: 9519848 B
'''
        sample = physical_footprint_sample(output, {65302, 65303})
        self.assertEqual(sample['physical_bytes'], 9519848)
        self.assertEqual(len(sample['processes']), 2)

    def test_an_exited_process_cannot_make_a_partial_sample_look_complete(self):
        output = 'Python [65302]: 64-bit    Footprint: 8274424 B\nSummary Footprint: 8274424 B\n'
        with self.assertRaises(ValueError):
            physical_footprint_sample(output, {65302, 65303})


if __name__ == '__main__':
    unittest.main()


class NativeStartTests(unittest.TestCase):
    def test_an_empty_devtools_port_file_is_not_ready_yet(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=SCRATCH, prefix='native-start-') as directory:
            root = Path(directory)
            browser = root / 'fake-chromium'
            browser.write_text(f'#!{sys.executable}\n' + FAKE_BROWSER)
            browser.chmod(0o755)
            args = argparse.Namespace(runtime='nanos', native_arg=[], native_binary=browser)
            trial = Trial(args, root / 'trial', 'native', resource_pass=False)
            try:
                trial.start()
                self.assertEqual(trial.version, 'Fake/1')
            finally:
                trial.close()
