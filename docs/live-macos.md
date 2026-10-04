# Live macOS acceptance

Run on 2026-10-03 in the logged-in desktop session, with real `WKWebView`
snapshots at 800x600. Both the local interactive page and an HTTPS page reached
Jackstay's SDL reference viewer and `katzensteg jackstay-source`.

| Component | Revision / version |
| --- | --- |
| Luchs implementation | `0275a8c4eeac13bf7b315fa6bf07020e7e88574d` |
| Jackstay Rust dependency and SDL viewer | `33369151429d82a2b923ab2891e199b05aca075a` |
| Katzensteg | `c981b7d482d57ddd0f16fae9b1c363b741a9b3a6` |
| Katzensteg's separately pinned Jackstay C library | `94a697e6ace998700fa7342b7bc7f217c496b367` (ABI 0.8) |
| OS | macOS 26.6, build 25G72, arm64 |
| Swift | Apple Swift 6.4, swiftlang-6.4.0.34.1 |
| Rust | 1.98.0, 88d9e12ae |

The implementation commit was rewritten after validation to separate the CI
workflow for governor delivery. Its Rust sources, helper and fixtures are byte
identical to the tested commit `0078490a99d62a8bb3f97ff0ff56f5dbec446c06`.

The existing Katzensteg launcher had Jackstay disabled. A clean scratch clone of
the same revision was built with `zig build -Djackstay=true
-Djackstay-prefix=/Users/robert/.cache/katzensteg/jackstay`. The prepared prefix
matched its committed dependency pin. No Katzensteg source was changed.
The reference viewer was built from the listed Jackstay revision with
`backend-macos`, using SDL 2.32.70.

## Commands and results

Each source ran unbounded so consumers could disconnect without stopping it:

```sh
LUCHS_CONSOLE_LOG=/tmp/interactive-console.log \
  target/debug/luchs --endpoint=luchs-live-interactive --size=800x600 testdata/interactive.html
LUCHS_CONSOLE_LOG=/tmp/https-console.log \
  target/debug/luchs --endpoint=luchs-live-https --size=800x600 https://example.com
```

For each printed socket path, the consumers ran:

```sh
capture-viewer-sdl --source-socket "$source_path" --observe --frames 300
katzensteg jackstay-source "$source_path" --observe
```

| Page | SDL result | Katzensteg result |
| --- | --- | --- |
| `testdata/interactive.html` | Exit 0, `acquired_frames=300`; visible fixture title and control panels | Live process, cleat render generation 54, `images=1/1` |
| `https://example.com` | Exit 0, `acquired_frames=300`; visible multilingual domain notice | Live process, cleat render generation 156, `images=1/1` |

The SDL windows were inspected through the desktop screenshot tool. Katzensteg
ran in a recorded cleat terminal with a functional Ghostty VT. Its render-packet
summaries showed the uploaded and placed image; its logs reported supported
graphics, successful shared-memory probing and direct-TTY fullscreen presentation.
This terminal evidence does not claim a kitty screenshot: the computer-use tool
refused access to that app.

Both WebKit console logs contained navigation completion, `requestAnimationFrame
fired`, and `after 2s: visibility visible, hasFocus false, requestAnimationFrame
ran`. The helper kept running after the bounded SDL consumers exited, and a
second SDL viewer connected successfully. SIGTERM then stopped each Luchs
process cleanly, reaped its helper and removed its source endpoint.

An initial live run found a missing `CpuCopyComplete` descriptor value, which
Katzensteg rejects. The implementation revision above fixes that field and adds
a regression assertion; the results in the table come from rerunning that build.

The test suite independently covers resize, holding an old lease across a
replacement, consumer process death with a held lease, reload through a fake
helper, and SIGTERM cleanup. Input delivery is outside this slice.

## Bootstrap v2 migration (2026-10-04)

Jackstay is now pinned to `ed785976df1246d2d3ce3f0c41df91c02f38e20d`
(ABI 0.12). The evidence above predates this migration and covers bootstrap v1.
The v2 live check remains pending: Jackstay #59, the SDL reference viewer's
bootstrap v2 slice, is still open. After it lands, present
`testdata/interactive.html` using that viewer on a live macOS desktop and append
the tested revisions and results here. Automated tests exercise v2 media, optional
controls, replacement, process death and SIGTERM with a live consumer.


## Framed helper protocol (2026-10-04)

Validated on the logged-in macOS 26.6 arm64 desktop with Apple Swift 6.4
(swiftlang-6.4.0.34.1) and Rust 1.98.0. Jackstay remains pinned to
`ed785976df1246d2d3ce3f0c41df91c02f38e20d` (ABI 0.12).

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
```

`native_ping_reload_and_watch` passed. It received real 32x32 WebKit frames,
confirmed `ping` and `reload` returned `Executed`, and confirmed the old
`mouse_down` command returned `Unsupported`. Rewriting a red page to blue and
reloading changed the first captured pixel from `[255, 0, 0, 255]` to
`[0, 0, 255, 255]`.

The test then launched the real CLI with `--watch --size=32x32 --fps=15`,
connected through bootstrap v2, and acquired both the original red pixels and
blue pixels after editing the file. This exercises file polling, framed stdin,
main-thread reload, ack demultiplexing, and publication through Jackstay. The
CLI stopped successfully on SIGTERM after six frames and removed its endpoint.
No SDL viewer was needed for this protocol acceptance check; the earlier v2
presenter check remains separate.


The review follow-up also ran `native_stdin_eof_exits_successfully`: a framed
ping with ID `18446744073709551615` round-tripped without losing bits, and closing
stdin exited the real Swift helper with status zero while a reader drained
stdout. The ping/reload/watch acceptance test passed again after the Rust
recovery changes. Fake-helper CLI tests separately cover retrying failed and
uncertain reloads without another file modification, and stopping on an
unsupported reload.


Both live tests passed once more after replacing raw pipe setup/readiness calls
with rustix wrappers and preserving command-write diagnostics. The final Rust
pipe path therefore has live ping/reload/watch and clean-EOF evidence as well as
the fake-helper regression tests.

## Socketpair, BGRA and idle capture (2026-10-04)

Validated on the same macOS 26.6 arm64 desktop (25G72), Apple Swift 6.4
(swiftlang-6.4.0.34.1), Rust 1.98.0, and SDL 2.32.70. The producer toolkit and
reference viewer use Jackstay `9e6f145e5f9f2d1ba5732b0672a5ba6fb7c4b9ad`
(PR #77). That toolkit revision returns consumed pixel storage and lets luchs
retain logical input geometry across scaled CPU allocation replacements.

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
cargo test --locked --test capture_policy --test helper_protocol
```

All four native tests passed: ping/reload/watch over the inherited socketpair,
clean socket EOF with a full-range u64 command ID, hidden/unchanged capture and
2x scale, and odd fractional pixel sizes. A 17x11 logical viewport produced
17x11, 26x17 and 34x22 frames at scale 1, 1.5 and 2, with native BGRA red pixels.
The last case caught AppKit rounding in `NSImage.cgImage`; drawing WebKit's
original image representation directly into the reused bitmap fixes it.

The fake-helper CLI test uses the real producer and presentation callback. It
checks unchanged publication sequences, two consecutive idle intervals of at
least 450 ms, wake within 200 ms after reload or a page signal, no capture
requests for 600 ms while hidden, immediate capture on re-show, a 2x pixel
allocation, and unchanged logical input dimensions. Frame decoder and toolkit
lifecycle tests check that recycled vectors retain their allocation pointers.

### SDL viewer and native editing

```sh
cargo build --workspace --locked --features backend-macos # in Jackstay
cmake -S tools/capture-viewer-sdl -B build/viewer
cmake --build build/viewer
# in luchs:
target/debug/luchs --stats --size=800x600 testdata/static.html
# pass its printed endpoint:
capture-viewer-sdl --source-socket "$source_path" --observe \
  --affordances optional --log-affordances
```

The production helper kept its transparent window ordered in. The live Retina
SDL viewer logged `source frame=800x600` followed by `source frame=1600x1200` as
its scale hint arrived. Desktop inspection showed the fixture title, click/key
readout and bar with sharp text. That static run completed 199 snapshots and
published just two frames (initial and 2x replacement); all 197 subsequent
snapshots were unchanged skips. Mean snapshot time was 3.989 ms and mean publish
time was 35.924 ms, including those two initial allocations. There were no later
publications after the idle threshold.

For native typing, a scratch build of the same Swift source made only its window
visible and mouse accepting, used a titled window and regular activation policy.
No capture, caret, transport or input script was changed. This made the helper's
own WebKit input field accessible to desktop automation without adding the later
Jackstay input slice. The CLI used `--helper` to select that scratch binary and
published `testdata/interactive.html` to the same SDL viewer. Typing `Frame skip`
and pressing Left updated the readout; subsequent Left presses moved the yellow
synthetic caret through the captured text. The viewer screenshot showed both the
typed value and caret between characters after its own window took focus. This
checks event-driven caret updates and page wake signals. It does not claim input
admission through the observation-only Jackstay channel.

An intermediate viewer launch failed with missing symbol
`ft_acquired_frame_macos_resources`: a local default-feature toolkit test build
had replaced `libjackstay.dylib` without `backend-macos`. Rebuilding with that
feature restored the viewer. The final live viewer inspection used the matching
native library and exited through source shutdown. Keep that feature enabled if
rebuilding Jackstay between live viewer runs.

### Timing measurements

The baseline was luchs `edf8a23dfb4500c8ed82809c25696c27832f41bf`'s
pre-change Swift helper, instrumented in a scratch file
around `takeSnapshot` and its complete emission function. It ran 90 snapshots at
30 fps over stdin/stdout; every snapshot was published. The static measurement
page was a dark background and one heading with no timers or CSS animations.
The updated CLI captured the same page for ten seconds at default `--fps=30`,
then exited on SIGTERM and printed `--stats`. The second size is a Retina-size
1600x1200 image; the separate SDL run above proves a scale hint reaches that
pixel size from an 800x600 logical viewport.

| Path | Pixels | Snapshots | Published | Unchanged skips | Mean snapshot ms | Mean publish ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Before, 90 samples | 800x600 | 90 | 90 | 0 | 2.279 | 10.317 |
| After, ten seconds | 800x600 | 33 | 1 | 32 | 2.422 | 27.461 |
| Before, 90 samples | 1600x1200 | 90 | 90 | 0 | 3.888 | 17.037 |
| After, ten seconds | 1600x1200 | 34 | 1 | 33 | 3.311 | 39.205 |

Snapshot time is WebKit API latency. Publish time includes drawing and transport;
the new path also hashes pixels. The after publish means each contain only one
cold-start publication, so they do not establish steady-state publish throughput.
These runs establish the removal of repeated static publications and the lower
idle snapshot rate. Direct arena writing and its copy-count measurement remain
#13's work. Small command/ack JSON and WebKit's own snapshot objects still
allocate; full-frame bitmap and receive buffers are reused after warm-up.

### Review regressions

The review follow-up replaces the 5 ms Rust receive poll with a deadline wait
interrupted by acknowledgement, state and presentation notifications. A fake
helper test verifies a 500 ms quiet wait, then prompt host, ack and page wakes.
The production watch/signal ceiling is 250 ms. Another subprocess test starts
with descriptors 0, 1 and 2 closed and verifies the inherited socket still
carries frames and acknowledgements. Visibility also applies when a requested
scale exceeds the capture limit.

The live macOS suite includes an HTTP server that deliberately leaves loading
unfinished for two seconds. Capture acknowledges without taking a snapshot;
ping, reload and presentation still complete within one second. Closing that
server without a response exercises the existing initial navigation failure
handlers and confirms that the helper exits with a diagnostic. Static pages now
stop the animation activity probe instead of running `getAnimations()` on every
animation frame. Snapshot completion also checks the window backing scale and
navigation state, discarding an invalidated snapshot and requesting a retry.

The final recovery policy allows two retries for consecutive invalid snapshots,
including WebKit errors or mismatched dimensions. The third reports the reason
to the CLI. `scripts/test-helper.sh` verifies that native policy, including budget
reset on success. The fake renderer discards two captures after an idle page
wake and verifies that no-report acknowledgements schedule the successful retry
with each next request within 250 ms instead of the 500 ms idle interval; another
CLI test verifies the exhausted-retry diagnostic.

Expected navigation or display-scale transitions discard an in-flight snapshot
without charging the failure budget. The native policy test covers that
distinction as well as genuine repeated failures. The helper build runs this
test by default for existing CI; local builders can set
`LUCHS_SKIP_NATIVE_TESTS=1` and run `scripts/test-helper.sh` separately.
