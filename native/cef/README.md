# Optional CEF renderer spike

The ordinary Luchs build and default renderer stay on the native WebView path.
Build the macOS arm64 experiment explicitly:

```sh
python3 scripts/build-cef.py --output target/cef/LuchsCEF.app
target/cef/LuchsCEF.app/Contents/MacOS/LuchsCEF --renderer cef --fps 60 page.html
```

The script downloads the pinned CEF distribution, verifies its published SHA1,
builds its sandbox-enabled wrapper, bundles the framework and subprocess helpers,
and signs the development bundle. It refuses to overwrite an existing bundle.
The SDK, wrapper and Cargo targets can be supplied explicitly. CEF is an optional
Cargo feature; normal builds neither download nor link it.

Every CEF run uses a fresh disposable profile. The browser and subprocesses use
Chromium's mock Keychain; Media Router and DIAL discovery are disabled. This
experiment must not use a real Chrome profile or ask for Chromium Safe Storage
access. Mock encryption is suitable only for these disposable test profiles.

## GPU path

Set `LUCHS_CEF_GPU=1` to use accelerated offscreen rendering. CEF lends an
IOSurface during `OnAcceleratedPaint`. Luchs copies it once on the GPU into a
Jackstay-owned arena resource and waits for that copy to complete before the
callback returns. Retaining CEF's IOSurface alone does not retain its pixels.
Consumers acquire Jackstay leases and wait/signal native readiness/completion
events; they do not take CPU snapshots of the page. The copy-completion CPU wait
is currently bounded at one second; failure terminates the experiment because
returning the borrowed surface with GPU reads outstanding would be unsafe.

`LUCHS_CEF_MACH_SERVICE` names the native acquisition service. Launch the producer
through a private launchd job declaring that Mach service. The normal `--endpoint`
still supplies input and page affordances. The companion Wheelhouse experiment
selects the Mach service through its explicit `macos_service` View setting.
Without a Mach service, the internal observer can still validate GPU ownership.

For visible interactive content, set `ProcessType` to `Interactive` in that job:

```xml
<key>ProcessType</key>
<string>Interactive</string>
```

Leaving it unset selects `Standard`, which permits CPU and I/O throttling even
when the machine is otherwise idle. In the M4 experiment this caused recurring
75–150 ms delivery gaps. Changing only the job classification restored 60 fps
through the complete GPU producer, including its copy-completion wait.
See Apple's [launchd job classifications](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5).

`LUCHS_CEF_DIAGNOSTICS=directory` samples frame 1, retains that lease across 59 more
publications, verifies that its bytes remain unchanged, verifies that frame 60
differs, and writes both PPMs plus `ownership.json`. This deliberately expensive
readback is off during ordinary runs. `LUCHS_CEF_TRACE=file` records up to 600
publications as timestamp_ns,width,height,callback_duration_ns,frame_count.
The duration includes publication and copy completion, not just the Metal blit.

Run a finite producer timing check on a quiet machine using a continuously
animating local fixture:

```sh
python3 scripts/profile-cef.py target/cef/LuchsCEF.app/Contents/MacOS/LuchsCEF \
  path/to/animation.html --output /tmp/luchs-cef-profile
```

The output directory must be new. The check runs 600 publications at 680×430,
discards the first ten, records callback and arrival timings, then removes its
private job. It requires at least 55 delivered fps and no gaps exceeding 33.4 ms.
Use `--process-type Standard` to reproduce the launch-classification problem.
These are producer measurements; they do not measure consumer display latency.

## Scope

This is a rendering/ownership experiment, not browser parity. CPU offscreen paint
is also available without `LUCHS_CEF_GPU`. The normal page navigation, geometry,
visibility and cooperative-input control plane is shared with Luchs. `--watch`
is rejected for this backend. Popup surfaces/windows, IME, complete platform key
mapping, cursor/scroll affordances, persistent sessions, other platforms and codec
coverage remain follow-up work. The prebuilt distribution does not promise all
proprietary codecs; the video fixture uses VP8 WebM.
