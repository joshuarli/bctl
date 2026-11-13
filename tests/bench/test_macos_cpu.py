import contextlib
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace
import unittest

from macos_cpu import snapshot, trial_snapshot
from macos_workflows import SCRATCH, Trial


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
        before = snapshot({os.getpid()})
        child = subprocess.Popen([sys.executable, '-c',
            'import signal,time\nt=time.process_time()+0.25\nwhile time.process_time()<t: pass\nprint(time.process_time(),flush=True)\nsignal.pause()'], stdout=subprocess.PIPE)
        try:
            expected = float(child.stdout.readline())
            both = snapshot({os.getpid(), child.pid})
            parent = snapshot({os.getpid()})
            self.assertIn(child.pid, parent['processes'])
            self.assertLess(abs(both['cpu_s'] - parent['cpu_s']), 0.05)
            self.assertGreaterEqual(parent['cpu_s'] - before['cpu_s'], expected)
            self.assertLess(parent['cpu_s'] - before['cpu_s'], expected + 0.15)
        finally:
            child.terminate()
            child.wait(timeout=5)
            child.stdout.close()

    def test_an_orphaned_runtime_is_charged_through_its_trial_directory(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='or-', dir=SCRATCH) as directory:
            root = Path(directory)
            image = root / 'image.img'
            image.write_bytes(b'image')
            image.with_suffix('.profile.img').write_bytes(b'profile')
            args = SimpleNamespace(runtime='nanos', native_arg=[], native_binary=Path('/unused'))
            trial = Trial(args, root / 't', 'bctl', resource_pass=False, image_path=image)
            child_code = '''import os,signal,socket,sys,time
target=time.process_time()+0.25
while time.process_time()<target: pass
s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(b'ready');signal.pause()
'''
            parent_code = 'import subprocess,sys\np=subprocess.Popen([sys.executable,"-c",sys.argv[1],sys.argv[2],sys.argv[3]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)\nprint(p.pid)'
            pid = None
            try:
                with socket.socket(socket.AF_UNIX) as server:
                    server.bind(str(root / 's'))
                    server.listen(1)
                    server.settimeout(5)
                    pid = int(subprocess.check_output([sys.executable, '-c', parent_code, child_code, root / 's', trial.path]))
                    with server.accept()[0] as connection:
                        connection.settimeout(5)
                        self.assertEqual(connection.recv(5), b'ready')
                        measured = trial_snapshot(trial)
                        own = snapshot({os.getpid()})
                        self.assertIn(pid, measured['processes'])
                        self.assertGreater(measured['cpu_s'] - own['cpu_s'], 0.20)
            finally:
                if pid is not None:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(pid, signal.SIGTERM)
                trial.close()


if __name__ == '__main__':
    unittest.main()
