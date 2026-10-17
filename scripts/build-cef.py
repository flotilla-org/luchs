#!/usr/bin/env python3
"""Build an optional, pinned CEF spike bundle; ordinary Cargo builds skip CEF."""
import argparse,hashlib,json,os,pathlib,platform,plistlib,shutil,subprocess,urllib.request,urllib.parse
ROOT=pathlib.Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--cef-root',type=pathlib.Path)
p.add_argument('--wrapper-lib',type=pathlib.Path)
p.add_argument('--output',type=pathlib.Path,default=ROOT/'target/cef/LuchsCEF.app')
p.add_argument('--target-dir',type=pathlib.Path,default=ROOT/'target/cef/cargo')
p.add_argument('--jobs',type=int,default=2)
a=p.parse_args()
if platform.system()!='Darwin' or platform.machine()!='arm64':p.error('The spike currently validates macOS arm64 only')
pin=json.loads((ROOT/'native/cef/pin.json').read_text())['macosarm64']
cache=ROOT/'target/cef';cache.mkdir(parents=True,exist_ok=True)
if a.cef_root:
 sdk=a.cef_root.resolve()
else:
 archive=cache/pin['archive']
 if not archive.exists():
  temporary=archive.with_suffix('.download')
  print('Downloading optional CEF SDK',flush=True)
  urllib.request.urlretrieve('https://cef-builds.spotifycdn.com/'+urllib.parse.quote(archive.name),temporary)
  temporary.rename(archive)
 if hashlib.sha1(archive.read_bytes()).hexdigest()!=pin['sha1']:raise RuntimeError('CEF archive checksum mismatch')
 sdk=cache/'sdk'/archive.name.removesuffix('.tar.bz2')
 if not sdk.exists():
  # Use macOS bsdtar so the builder also works with Xcode's Python 3.9.
  # The verified archive is the pinned CEF distribution, including framework links.
  (cache/'sdk').mkdir(exist_ok=True)
  subprocess.run(['/usr/bin/tar','-xjf',str(archive),'-C',str(cache/'sdk')],check=True)
if not (sdk/'include/cef_app.h').exists():raise RuntimeError('Not a CEF binary distribution')
wrapper=a.wrapper_lib.resolve() if a.wrapper_lib else cache/'wrapper/libcef_dll_wrapper'
if not (wrapper/'libcef_dll_wrapper.a').exists():
 subprocess.run(['cmake','-S',str(sdk),'-B',str(wrapper.parent),'-G','Ninja','-DCMAKE_BUILD_TYPE=Release','-DPROJECT_ARCH=arm64','-DUSE_SANDBOX=ON'],check=True)
 subprocess.run(['cmake','--build',str(wrapper.parent),'--target','libcef_dll_wrapper','-j',str(a.jobs)],check=True)
env=dict(os.environ,CEF_ROOT=str(sdk),CEF_WRAPPER_LIB=str(wrapper))
subprocess.run(['cargo','build','--release','--locked','--features','cef','--target-dir',str(a.target_dir)],cwd=ROOT,env=env,check=True)
bundle=a.output.resolve()
if bundle.exists():raise RuntimeError(f'Refusing to overwrite an existing bundle: {bundle}')
contents=bundle/'Contents';frameworks=contents/'Frameworks';macos=contents/'MacOS';macos.mkdir(parents=True);frameworks.mkdir()
shutil.copy2(a.target_dir/'release/luchs',macos/'LuchsCEF')
shutil.copytree(sdk/'Release/Chromium Embedded Framework.framework',frameworks/'Chromium Embedded Framework.framework',symlinks=True)
info={'CFBundleIdentifier':'work.flotilla.luchs.cef-spike','CFBundleName':'LuchsCEF','CFBundleExecutable':'LuchsCEF','CFBundlePackageType':'APPL','CFBundleVersion':'1','LSUIElement':True,'NSHighResolutionCapable':True}
(contents/'Info.plist').write_bytes(plistlib.dumps(info))
helper=cache/'cef-helper'
subprocess.run(['clang++','-std=c++20','-fobjc-arc','-O2','-DCEF_USE_SANDBOX=1','-I',str(sdk),str(ROOT/'native/cef/helper.mm'),str(wrapper/'libcef_dll_wrapper.a'),'-framework','AppKit','-o',str(helper)],check=True)
entitlements=cache/'entitlements.plist'
entitlements.write_bytes(plistlib.dumps({'com.apple.security.cs.allow-jit':True,'com.apple.security.cs.allow-unsigned-executable-memory':True,'com.apple.security.cs.disable-library-validation':True,'com.apple.security.get-task-allow':True}))
for suffix in ['', ' (Alerts)',' (GPU)',' (Plugin)',' (Renderer)']:
 name='LuchsCEF Helper'+suffix;helper_bundle=frameworks/(name+'.app');directory=helper_bundle/'Contents/MacOS';directory.mkdir(parents=True)
 shutil.copy2(helper,directory/name)
 helper_info=dict(info,CFBundleName=name,CFBundleExecutable=name,CFBundleIdentifier='work.flotilla.luchs.cef-spike.helper'+suffix.replace(' ','').replace('(','-').replace(')','').lower())
 (helper_bundle/'Contents/Info.plist').write_bytes(plistlib.dumps(helper_info))
 subprocess.run(['codesign','--force','--sign','-','--entitlements',str(entitlements),str(helper_bundle)],check=True)
subprocess.run(['codesign','--force','--deep','--sign','-','--entitlements',str(entitlements),str(bundle)],check=True)
print(bundle/'Contents/MacOS/LuchsCEF',flush=True)
