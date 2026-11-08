#!/usr/bin/env python3
import hashlib
import io
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


class GuestPackageLockTests(unittest.TestCase):
    def test_corrupt_cached_apk_is_rejected_before_package_installation(self):
        with tempfile.TemporaryDirectory(dir=ROOT / '.work/tmp') as directory:
            fixture = pathlib.Path(directory)
            (fixture / 'harness').mkdir()
            (fixture / 'guest').mkdir()
            (fixture / 'bin').mkdir()
            cache = fixture / '.work/dl'
            cache.mkdir(parents=True)
            tarball = cache / 'alpine-minirootfs-3.20.3-x86_64.tar.gz'
            release = b'3.20.3\n'
            with tarfile.open(tarball, 'w:gz') as archive:
                entry = tarfile.TarInfo('etc/alpine-release')
                entry.size = len(release)
                archive.addfile(entry, io.BytesIO(release))
            script = (ROOT / 'harness/prepare-guest.sh').read_text()
            pinned_tarball = 'd4e6fd67dcf75e40c451560ac7265166c2b72a0f38ddc9aae756a7de3d1efa0c'
            self.assertEqual(script.count(pinned_tarball), 1)
            script = script.replace(pinned_tarball, hashlib.sha256(tarball.read_bytes()).hexdigest())
            (fixture / 'harness/prepare-guest.sh').write_text(script)
            name = 'main/fixture-1.apk'
            package = cache / 'apk' / name
            package.parent.mkdir(parents=True)
            original = b'fixture package bytes\n'
            package.write_bytes(original)
            digest = hashlib.sha256(original).hexdigest()
            (fixture / 'guest/packages.sha256').write_text(f'{digest}  {name}\n')
            apk = fixture / 'bin/apk'
            apk.write_text('#!/bin/sh\nprintf "%s\\n" APK_INVOKED\n')
            apk.chmod(0o755)
            environment = dict(os.environ, PATH=f'{fixture / "bin"}:{os.environ["PATH"]}')
            command = ['bash', 'harness/prepare-guest.sh', str(fixture / '.work/guest-root')]
            valid = subprocess.run(command, cwd=fixture, env=environment,
                                   capture_output=True, text=True, timeout=30)
            self.assertEqual(valid.returncode, 0, valid.stdout + valid.stderr)
            self.assertIn('APK_INVOKED', valid.stdout)
            package.write_bytes(original[:-1] + bytes([original[-1] ^ 1]))
            corrupt = subprocess.run(command, cwd=fixture, env=environment,
                                     capture_output=True, text=True, timeout=30)
            self.assertNotEqual(corrupt.returncode, 0)
            self.assertIn(f'{name}: FAILED', corrupt.stdout)
            self.assertNotIn('APK_INVOKED', corrupt.stdout)
            self.assertNotIn('GUEST_PACKAGES_VERIFIED', corrupt.stdout)


if __name__ == '__main__':
    unittest.main()
