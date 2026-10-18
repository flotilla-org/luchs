#!/usr/bin/env python3
"""Exercise CEF setup-error profile cleanup before native initialization."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=Path, help='built CEF bundle executable')
args = parser.parse_args()
binary = args.binary.resolve(strict=True)
# Local Endpoint paths must fit sockaddr_un, so keep TMPDIR short.
with tempfile.TemporaryDirectory(prefix='lc-', dir='/tmp') as folder:
    folder = Path(folder)
    page = folder / 'page.html'
    page.write_text('<!doctype html><title>setup cleanup</title>')
    env = dict(os.environ, TMPDIR=str(folder), LUCHS_CEF_TRACE=str(folder))
    result = subprocess.run([
        str(binary), '--renderer', 'cef', '--endpoint', 'cache-check', str(page),
    ], env=env, capture_output=True, text=True, timeout=15)
    assert result.returncode != 0, 'invalid trace target unexpectedly succeeded'
    assert 'Is a directory' in result.stderr, result.stderr
    leaked = list(folder.glob('luchs-cef-*'))
    assert not leaked, f'failed CEF setup leaked profiles: {leaked}'
print('CEF setup-error profile cleanup passed')
