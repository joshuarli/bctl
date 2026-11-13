import contextlib
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import unittest

from macos_workflows import SCRATCH, invoke


class ProcessTests(unittest.TestCase):
    def test_a_timed_out_command_also_signals_its_child(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='iv-', dir=SCRATCH) as directory:
            path = Path(directory) / 's'
            with socket.socket(socket.AF_UNIX) as server:
                server.bind(str(path))
                server.listen(1)
                server.settimeout(5)
                child = '''import os,signal,socket,sys
s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1])
def stopped(*_):
 s.sendall(b'stopped');sys.exit(0)
signal.signal(signal.SIGTERM,stopped)
s.sendall((str(os.getpid())+'\\n').encode());signal.pause()
'''
                parent = 'import subprocess,signal,sys\nsubprocess.Popen([sys.executable,"-c",sys.argv[1],sys.argv[2]])\nsignal.pause()'
                errors = []
                def run():
                    try:
                        invoke([sys.executable, '-c', parent, child, path], timeout=1)
                    except BaseException as error:
                        errors.append(error)
                thread = threading.Thread(target=run)
                thread.start()
                pid = None
                try:
                    with server.accept()[0] as connection:
                        connection.settimeout(5)
                        stream = connection.makefile('rb')
                        pid = int(stream.readline())
                        self.assertEqual(stream.read(7), b'stopped')
                        stream.close()
                    thread.join(timeout=5)
                    self.assertFalse(thread.is_alive())
                    self.assertEqual(len(errors), 1)
                    self.assertIsInstance(errors[0], subprocess.TimeoutExpired)
                finally:
                    if pid is not None:
                        with contextlib.suppress(ProcessLookupError):
                            os.kill(pid, signal.SIGKILL)
                    thread.join(timeout=5)

    def test_termination_unwinds_then_preserves_signal_death(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='sig-', dir=SCRATCH) as directory:
            marker = Path(directory) / 'cleaned'
            code = '''import signal,sys
from pathlib import Path
sys.path.insert(0,sys.argv[1])
from macos_workflows import interruptions
with interruptions():
 try:
  print('ready',flush=True);signal.pause()
 finally:
  Path(sys.argv[2]).write_text('cleaned')
'''
            process = subprocess.Popen([sys.executable, '-c', code, Path(__file__).parent, marker], stdout=subprocess.PIPE)
            try:
                self.assertEqual(process.stdout.readline(), b'ready\n')
                process.terminate()
                self.assertEqual(process.wait(timeout=5), -signal.SIGTERM)
                self.assertEqual(marker.read_text(), 'cleaned')
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                process.stdout.close()


if __name__ == '__main__':
    unittest.main()
