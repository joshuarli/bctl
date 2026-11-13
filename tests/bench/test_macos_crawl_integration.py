import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest

from macos_workflows import ROOT, SCRATCH, stop


@unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('BCTL_BENCH_INTEGRATION') == '1',
                     'requires macOS browsers and BCTL_BENCH_INTEGRATION=1')
class CrawlIntegrationTests(unittest.TestCase):
    def test_budget_overrun_exits_nonzero_after_capture_and_replay_complete(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='budget-', dir=SCRATCH) as directory:
            receipt = Path(directory) / 'result.json'
            process = subprocess.Popen([sys.executable, ROOT / 'tests/bench/macos_crawl.py',
                '--pages', '1', '--jobs', '1', '--rounds', '0', '--resource-rounds', '1',
                '--budget-mib', '1', '--native-arg=--disable-gpu', '--out', receipt],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
            try:
                stdout, stderr = process.communicate(timeout=180)
                self.assertEqual(process.returncode, 1, stdout + stderr)
                results = json.loads(receipt.read_text())
                self.assertTrue(results['completed'])
                self.assertEqual(len(results['attempts']), 2)
                for attempt in results['attempts']:
                    self.assertEqual(attempt['status'], 'over_budget')
                    self.assertGreater(attempt['sampled_peak_physical_bytes'], 1 << 20)
                    self.assertFalse(attempt['within_sampled_budget'])
                    self.assertEqual(attempt['workers'][0]['status'], 'pass')
                    self.assertEqual(attempt['workers'][0]['verification']['pages_passed'], 1)
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGINT)
                    try:
                        process.communicate(timeout=45)
                    except subprocess.TimeoutExpired:
                        stop(process)
                        process.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
