"""Actual Linux archive provenance/determinism, using an already built release."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get('PACKAGE_TEST_BINARY', ROOT / 'target/release/omarchy-photo-wallpapers'))


@unittest.skipUnless(sys.platform.startswith('linux') and BINARY.is_file(), 'requires a built Linux release binary')
class PackageTests(unittest.TestCase):
    def package(self, out, commit='1' * 40):
        return subprocess.run([sys.executable, str(ROOT / 'scripts/package-release'), '--binary', str(BINARY), '--output', str(out), '--source-commit', commit], env=os.environ | {'SOURCE_DATE_EPOCH': '1700000000'}, capture_output=True, text=True)

    def test_explicit_source_identity_and_normalized_repacking(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / 'first', Path(directory) / 'second'
            for output in (first, second):
                result = self.package(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                build = json.loads((output / 'BUILD.json').read_text())
                self.assertEqual(build['source_commit'], '1' * 40)
                self.assertEqual(build['source_date_epoch'], 1700000000)
            a = next(first.glob('*.tar.gz')); b = next(second.glob('*.tar.gz'))
            self.assertEqual(hashlib.sha256(a.read_bytes()).hexdigest(), hashlib.sha256(b.read_bytes()).hexdigest())

    def test_bad_identity_and_existing_destination_cannot_overwrite_files(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'valuable'; out.mkdir(); (out / 'keep').write_text('preserve')
            self.assertNotEqual(self.package(out).returncode, 0)
            self.assertEqual(list(out.iterdir()), [out / 'keep'])
            self.assertEqual((out / 'keep').read_text(), 'preserve')
            invalid = self.package(Path(directory) / 'invalid', 'mutable-main')
            self.assertNotEqual(invalid.returncode, 0)
            self.assertFalse((Path(directory) / 'invalid').exists())


if __name__ == '__main__':
    unittest.main()
