import json
import os
import subprocess
import sys
import unittest

from macos_cpu import snapshot


@unittest.skipUnless(sys.platform == 'darwin', 'requires macOS libproc')
class CpuTests(unittest.TestCase):
    def test_cpu_survives_child_reaping_and_uses_seconds(self):
        before = snapshot({os.getpid()})
        result = subprocess.check_output([sys.executable, '-c',
            'import json,time\nstart=time.process_time()\nwhile time.process_time()-start<0.25: pass\nprint(json.dumps(time.process_time()-start))'])
        after = snapshot({os.getpid()})
        expected = json.loads(result)
        actual = after['cpu_s'] - before['cpu_s']
        self.assertGreaterEqual(actual, expected)
        self.assertLess(actual, expected + 0.15)

    def test_overlapping_roots_do_not_double_count_a_live_child(self):
        child = subprocess.Popen([sys.executable, '-c',
            'import signal,time\nt=time.process_time()+0.25\nwhile time.process_time()<t: pass\nprint("ready",flush=True)\nsignal.pause()'], stdout=subprocess.PIPE)
        try:
            self.assertEqual(child.stdout.readline(), b'ready\n')
            both = snapshot({os.getpid(), child.pid})
            parent = snapshot({os.getpid()})
            self.assertIn(child.pid, parent['processes'])
            self.assertLess(abs(both['cpu_s'] - parent['cpu_s']), 0.05)
        finally:
            child.terminate()
            child.wait(timeout=5)
            child.stdout.close()


if __name__ == '__main__':
    unittest.main()
