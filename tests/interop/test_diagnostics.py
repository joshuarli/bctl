import ast
import contextlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class DiagnosticsTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(dir=ROOT / '.work/tmp')
        self.addCleanup(self.scratch.cleanup)
        self.home = Path(self.scratch.name) / 'home'
        machine = self.home / 'machines/test'
        machine.mkdir(parents=True)
        (machine / 'console.log').write_text('guest fault\n')
        (self.home / 'diagnostics').write_text('blocks directory creation')
        self.bctl = Path(self.scratch.name) / 'bctl'
        self.bctl.write_text('#!/bin/sh\nprintf stopped >"$BCTL_HOME/stopped"\n')
        self.bctl.chmod(0o700)

    def test_down_survives_snapshot_failure(self):
        tree = ast.parse((ROOT / 'tests/interop/checks.py').read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'bctl')
        namespace = dict(os=os, Path=Path, shutil=shutil, subprocess=subprocess, sys=sys, redact=lambda text: text)
        exec(compile(ast.Module(body=[function], type_ignores=[]), 'checks.py', 'exec'), namespace)
        previous = {key: os.environ.get(key) for key in ('BCTL', 'BCTL_HOME')}
        try:
            os.environ.update(BCTL=str(self.bctl), BCTL_HOME=str(self.home))
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                result = namespace['bctl']('down', 'test')
            self.assertEqual(result.returncode, 0)
            self.assertEqual((self.home / 'stopped').read_text(), 'stopped')
            self.assertIn('cannot preserve', errors.getvalue())
        finally:
            for key, value in previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value

    def test_cleanup_preserves_failure_and_stops_after_snapshot_error(self):
        self.check_cleanup(False)

    def test_cleanup_reports_original_home_when_retention_fails(self):
        self.check_cleanup(True)

    def check_cleanup(self, block_retention):
        work = Path(self.scratch.name) / 'work'
        work.mkdir()
        if block_retention:
            (work / 'failures').write_text('blocks retention')
        source = (ROOT / 'tests/interop/run.sh').read_text()
        cleanup = source[source.index('cleanup() {'):source.index('trap cleanup EXIT')]
        result = subprocess.run(['sh', '-c', 'set -eu\n' + cleanup + '\ntrap cleanup EXIT\nexit 37\n'],
            env={**os.environ, 'home': str(self.home), 'work': str(work), 'bctl': str(self.bctl),
                 'BCTL_HOME': str(self.home), 'session': 'test', 'fixture_pid': ''},
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 37)
        retained = self.home if block_retention else next((work / 'failures').iterdir())
        self.assertEqual((retained / 'stopped').read_text(), 'stopped')
        self.assertEqual((retained / 'machines/test/console.log').read_text(), 'guest fault\n')
        self.assertIn(f'failure evidence: {retained}', result.stderr)


if __name__ == '__main__':
    unittest.main()
