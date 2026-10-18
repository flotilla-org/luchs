#!/usr/bin/env python3
"""Build an optional, pinned CEF spike bundle; ordinary Cargo builds skip CEF."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tempfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def verify_archive(archive, pin):
    sha1, sha256 = hashlib.sha1(), hashlib.sha256()
    with archive.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            sha1.update(chunk)
            sha256.update(chunk)
    if sha1.hexdigest() != pin['sha1'] or sha256.hexdigest() != pin['sha256']:
        raise RuntimeError('CEF archive checksum mismatch')


def cached_sdk(cache, pin):
    archive = cache / pin['archive']
    if not archive.exists():
        temporary = archive.with_suffix('.download')
        print('Downloading optional CEF SDK', flush=True)
        url = 'https://cef-builds.spotifycdn.com/' + urllib.parse.quote(archive.name)
        try:
            with urllib.request.urlopen(url, timeout=60) as response, temporary.open('wb') as output:
                shutil.copyfileobj(response, output)
            verify_archive(temporary, pin)
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    verify_archive(archive, pin)
    parent = cache / 'sdk'
    parent.mkdir(exist_ok=True)
    sdk = parent / archive.name.removesuffix('.tar.bz2')
    marker = sdk / '.luchs-extraction-complete'
    if not marker.exists() or marker.read_text() != pin['sha256']:
        # Publish only a complete extraction. A failed tar leaves the old SDK
        # intact; an unmarked/partial cache is replaced after success.
        with tempfile.TemporaryDirectory(prefix='extract-', dir=parent) as staging:
            staging = Path(staging)
            subprocess.run(['/usr/bin/tar', '-xjf', str(archive), '-C', str(staging)], check=True)
            extracted = staging / sdk.name
            if not (extracted / 'include/cef_app.h').is_file():
                raise RuntimeError('Archive is not a CEF binary distribution')
            (extracted / '.luchs-extraction-complete').write_text(pin['sha256'])
            previous = staging / 'previous'
            if sdk.exists():
                sdk.rename(previous)
            try:
                extracted.rename(sdk)
            except BaseException:
                if previous.exists():
                    previous.rename(sdk)
                raise
    return sdk


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cef-root', type=Path)
    parser.add_argument('--wrapper-lib', type=Path)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/cef/LuchsCEF.app')
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'target/cef/cargo')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    args.target_dir = args.target_dir.resolve()
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        parser.error('The spike currently validates macOS arm64 only')
    bundle = args.output.resolve()
    if bundle.exists():
        raise RuntimeError(f'Refusing to overwrite an existing bundle: {bundle}')
    pin = json.loads((ROOT / 'native/cef/pin.json').read_text())['macosarm64']
    cache = ROOT / 'target/cef'
    cache.mkdir(parents=True, exist_ok=True)
    sdk = args.cef_root.resolve() if args.cef_root else cached_sdk(cache, pin)
    if not (sdk / 'include/cef_app.h').is_file():
        raise RuntimeError('Not a CEF binary distribution')
    wrapper = args.wrapper_lib.resolve() if args.wrapper_lib else cache / 'wrapper/libcef_dll_wrapper'
    if not (wrapper / 'libcef_dll_wrapper.a').exists():
        subprocess.run([
            'cmake', '-S', str(sdk), '-B', str(wrapper.parent), '-G', 'Ninja',
            '-DCMAKE_BUILD_TYPE=Release', '-DPROJECT_ARCH=arm64', '-DUSE_SANDBOX=ON',
        ], check=True)
        subprocess.run([
            'cmake', '--build', str(wrapper.parent), '--target', 'libcef_dll_wrapper',
            '-j', str(args.jobs),
        ], check=True)
    env = dict(os.environ, CEF_ROOT=str(sdk), CEF_WRAPPER_LIB=str(wrapper))
    subprocess.run([
        'cargo', 'build', '--release', '--locked', '--features', 'cef',
        '--target-dir', str(args.target_dir),
    ], cwd=ROOT, env=env, check=True)
    contents = bundle / 'Contents'
    frameworks, macos = contents / 'Frameworks', contents / 'MacOS'
    macos.mkdir(parents=True)
    frameworks.mkdir()
    shutil.copy2(args.target_dir / 'release/luchs', macos / 'LuchsCEF')
    shutil.copytree(
        sdk / 'Release/Chromium Embedded Framework.framework',
        frameworks / 'Chromium Embedded Framework.framework', symlinks=True,
    )
    info = {
        'CFBundleIdentifier': 'work.flotilla.luchs.cef-spike',
        'CFBundleName': 'LuchsCEF', 'CFBundleExecutable': 'LuchsCEF',
        'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1',
        'LSUIElement': True, 'NSHighResolutionCapable': True,
    }
    (contents / 'Info.plist').write_bytes(plistlib.dumps(info))
    helper = cache / 'cef-helper'
    subprocess.run([
        'clang++', '-std=c++20', '-fobjc-arc', '-O2', '-DCEF_USE_SANDBOX=1',
        '-I', str(sdk), str(ROOT / 'native/cef/helper.mm'),
        str(wrapper / 'libcef_dll_wrapper.a'), '-framework', 'AppKit', '-o', str(helper),
    ], check=True)
    entitlements = cache / 'entitlements.plist'
    entitlements.write_bytes(plistlib.dumps({
        'com.apple.security.cs.allow-jit': True,
        'com.apple.security.cs.allow-unsigned-executable-memory': True,
        'com.apple.security.cs.disable-library-validation': True,
        'com.apple.security.get-task-allow': True,
    }))
    for suffix in ['', ' (Alerts)', ' (GPU)', ' (Plugin)', ' (Renderer)']:
        name = 'LuchsCEF Helper' + suffix
        helper_bundle = frameworks / (name + '.app')
        directory = helper_bundle / 'Contents/MacOS'
        directory.mkdir(parents=True)
        shutil.copy2(helper, directory / name)
        identifier = suffix.replace(' ', '').replace('(', '-').replace(')', '').lower()
        helper_info = dict(
            info, CFBundleName=name, CFBundleExecutable=name,
            CFBundleIdentifier='work.flotilla.luchs.cef-spike.helper' + identifier,
        )
        (helper_bundle / 'Contents/Info.plist').write_bytes(plistlib.dumps(helper_info))
        subprocess.run([
            'codesign', '--force', '--sign', '-', '--entitlements', str(entitlements), str(helper_bundle),
        ], check=True)
    subprocess.run([
        'codesign', '--force', '--deep', '--sign', '-', '--entitlements', str(entitlements), str(bundle),
    ], check=True)
    print(bundle / 'Contents/MacOS/LuchsCEF', flush=True)


if __name__ == '__main__':
    main()
