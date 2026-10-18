#!/usr/bin/env python3
"""Check SDK extraction/recovery without downloading or building CEF."""

import hashlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('build_cef', Path(__file__).with_name('build-cef.py'))
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class ExtractionTests(unittest.TestCase):
    def fixture(self, cache, valid=True):
        archive = cache / 'cef-fixture.tar.bz2'
        if valid:
            with tarfile.open(archive, 'w:bz2') as output:
                header = tarfile.TarInfo('cef-fixture/include/cef_app.h')
                content = b'complete SDK header'
                header.size = len(content)
                output.addfile(header, io.BytesIO(content))
        else:
            archive.write_bytes(b'not a tar archive')
        content = archive.read_bytes()
        return dict(archive=archive.name, sha1=hashlib.sha1(content).hexdigest(),
                    sha256=hashlib.sha256(content).hexdigest())

    def test_partial_cache_is_replaced_only_after_complete_extraction(self):
        with tempfile.TemporaryDirectory() as folder:
            cache = Path(folder)
            pin = self.fixture(cache)
            partial = cache / 'sdk/cef-fixture'
            partial.mkdir(parents=True)
            (partial / 'partial').write_text('unfinished')
            sdk = builder.cached_sdk(cache, pin)
            self.assertEqual((sdk / 'include/cef_app.h').read_text(), 'complete SDK header')
            self.assertFalse((sdk / 'partial').exists())
            self.assertEqual((sdk / '.luchs-extraction-complete').read_text(), pin['sha256'])
            self.assertEqual(builder.cached_sdk(cache, pin), sdk)

    def test_failed_extraction_preserves_previous_cache(self):
        with tempfile.TemporaryDirectory() as folder:
            cache = Path(folder)
            pin = self.fixture(cache, valid=False)
            previous = cache / 'sdk/cef-fixture'
            previous.mkdir(parents=True)
            (previous / 'previous').write_text('retained')
            with self.assertRaises(subprocess.CalledProcessError):
                builder.cached_sdk(cache, pin)
            self.assertEqual((previous / 'previous').read_text(), 'retained')
            self.assertFalse((previous / '.luchs-extraction-complete').exists())

    def test_corrupt_cached_archive_is_rejected_before_extraction(self):
        with tempfile.TemporaryDirectory() as folder:
            cache = Path(folder)
            pin = self.fixture(cache)
            (cache / pin['archive']).write_bytes(b'corrupt cached download')
            with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                builder.cached_sdk(cache, pin)
            self.assertFalse((cache / 'sdk').exists())


if __name__ == '__main__':
    unittest.main()
